// The host's media player: what the table's `player_open` and `player_verb` do (rust-modules/platform-tizen/include/plx_host.h, PLAYER).
//
// THE SHAPE, from the header's rules.
//  * A request RETURNS AT ONCE. OpenNative and VerbNative run on the library's loop thread; they validate, copy, enqueue and return. Everything
//    that touches the native player runs on ONE video thread (`plx-player`) that consumes the queue in order, so no player call ever runs on the
//    loop thread or on the Ecore main thread (the one exception, binding the display, is dispatched to the main thread and waited for with a
//    bound: see BindDisplay).
//  * Results are EVENTS, posted with plx_host_post from whatever thread has them: the video thread, the position poller or a native callback.
//    Every event carries the epoch the library minted for the open, and every native callback is registered with that epoch as its `user_data`,
//    so a late callback names its own player; the library drops an event whose epoch is no longer live (the nuvio session-counter rule, Video.cs).
//  * Position is POLLED here, about every 250 ms while playing, on a thread of its own and off the request queue: the native read can take
//    seconds on some firmware, and a read that has not returned after StallMs posts PLAYER_STALL once. There is no synchronous position call.
//
// MEASURED on the emulator 2026-11-22 (TV 10.0 x86_64; a probe on a scratch branch, a local clip): prepared, buffering and completed callbacks fire
// on ONE native thread per player instance that is not the main thread (a new thread for each player), so they only post events and never touch
// managed UI state; a player with no display does not advance; the completed callback fires with the native state still PLAYING. The thread the
// callbacks use on a Samsung set is UNVERIFIED, and the design does not depend on it.
//
// WHAT THE HOST NEVER DOES HERE: log the uri (it carries the Plex token), destroy a player a position read is inside (the handle is guarded;
// a stuck read leaks the handle and the process exit is the teardown), or leave a player behind: CLOSE always ends in exactly one PLAYER_CLOSED.

using System;
using System.Collections.Concurrent;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;
using System.Threading.Tasks;

namespace PlxHost
{
    internal sealed class PlxPlayer
    {
        internal static readonly PlxPlayer Instance = new PlxPlayer();

        private const int QueueLimit = 64;
        private const int PollMs = 250;
        private const int StallMs = 3000;
        private const int BindWaitMs = 3000;
        private const int CloseWaitMs = 3000;

        // The window the library made is the first Ecore_Wl2 window (id 1) on the connected display: the library makes exactly one. Measured on
        // the emulator to be the very pointer the library logs ("ecore: window_new(...) -> 0x..."); BindDisplay logs the pointer it found, so the
        // two lines can be compared in the UDP log. If a later change makes a second window first, this id is the line to revisit.
        private const uint LibraryWindowId = 1;

        // One player's host-side state. Video thread owns everything except what is marked.
        private sealed class Session
        {
            internal readonly uint Epoch;
            internal IntPtr Handle;
            internal bool Prepared;
            internal bool PlayPending;
            internal volatile bool Playing; // read by the poller
            internal volatile bool Closing; // read by the poller and the main-thread bind
            internal bool StallSent; // poller only
            // Seeks in flight, oldest first: the native completion callback carries no command id, and completions arrive in order.
            internal readonly List<long[]> Seeks = new List<long[]>(); // { command id, target ms }
            // Held by anything that calls into the native player on this handle off the video thread (position reads, the display bind). The
            // video thread takes it, with a bound, before it destroys the handle.
            internal readonly object HandleGate = new object();

            internal Session(uint epoch) { Epoch = epoch; }
        }

        private readonly BlockingCollection<Action> queue = new BlockingCollection<Action>(QueueLimit);
        private readonly object startGate = new object();
        private Thread videoThread;
        private Thread pollThread;
        private volatile bool shuttingDown;
        private volatile bool threadsFailed;

        private Session current; // video thread
        private volatile Session published; // the poller's view of `current`
        private long latestEpoch; // the epoch of the last open; written on the loop thread, read there (Interlocked)

        // The native callbacks are static, so nothing but the end of the process lets the runtime collect their thunks while a player holds them.
        private static readonly MediaPlayerNative.VoidCallback PreparedCb = user => Instance.Enqueue(EpochOf(user), s => Instance.OnPrepared(s));
        private static readonly MediaPlayerNative.VoidCallback CompletedCb = user => Instance.Enqueue(EpochOf(user), s => Instance.OnCompleted(s));
        private static readonly MediaPlayerNative.VoidCallback SeekDoneCb = user => Instance.Enqueue(EpochOf(user), s => Instance.OnSeekDone(s));
        private static readonly MediaPlayerNative.IntCallback ErrorCb =
            (code, user) => PlxApp.PostPlayer((uint)Native.PLX_HOST_EVENT_PLAYER_ERROR, EpochOf(user), code, 0);
        private static readonly MediaPlayerNative.IntCallback BufferingCb =
            (percent, user) => PlxApp.PostPlayer((uint)Native.PLX_HOST_EVENT_PLAYER_BUFFERING, EpochOf(user), percent, 0);
        private static readonly MediaPlayerNative.IntCallback InterruptedCb =
            (code, user) => PlxApp.PostPlayer((uint)Native.PLX_HOST_EVENT_PLAYER_INTERRUPTED, EpochOf(user), code, 0);

        private static IntPtr UserOf(uint epoch) { return new IntPtr(unchecked((int)epoch)); }
        private static uint EpochOf(IntPtr user) { return unchecked((uint)user.ToInt64()); }

        // Whether the device has the media player library at all. Called once at start; false leaves the table's player pointers NULL (the
        // library then plays nothing and says so), and `note` is the line for the log.
        internal static bool Probe(out string note)
        {
            IntPtr h = Dl.dlopen(MediaPlayerNative.MediaLib, Dl.RTLD_NOW);
            if (h != IntPtr.Zero)
            {
                note = "media player: " + MediaPlayerNative.MediaLib + " opened; the table offers player_open and player_verb";
                return true;
            }
            IntPtr err = Dl.dlerror();
            note = "media player: " + MediaPlayerNative.MediaLib + " did not open (" + (err == IntPtr.Zero ? "no dlerror" : Marshal.PtrToStringAnsi(err))
                + "); the table leaves the player pointers NULL, so the library plays nothing";
            return false;
        }

        // ---- the table's two requests (loop thread): validate, enqueue, return --------------------------------------------------------------

        // PlxPlayerOpen.
        internal static int OpenNative(uint epoch, IntPtr uri, int x, int y, int w, int h)
        {
            try
            {
                if (epoch == 0 || uri == IntPtr.Zero) return Native.PLX_HOST_PLAYER_E_ARGS;
                string text = ReadUtf8(uri);
                if (text.Length == 0) return Native.PLX_HOST_PLAYER_E_ARGS;
                return Instance.Open(epoch, text, x, y, w, h);
            }
            catch (Exception e)
            {
                PlxApp.Log("player_open: " + e.GetType().Name + ": " + e.Message);
                return Native.PLX_HOST_PLAYER_E_UNAVAILABLE;
            }
        }

        // PlxPlayerVerb.
        internal static int VerbNative(uint epoch, uint verb, long arg0, long arg1)
        {
            try
            {
                return Instance.Verb(epoch, verb, arg0, arg1);
            }
            catch (Exception e)
            {
                PlxApp.Log("player_verb: " + e.GetType().Name + ": " + e.Message);
                return Native.PLX_HOST_PLAYER_E_UNAVAILABLE;
            }
        }

        // Marshal.PtrToStringUTF8 is not in the netstandard2.0 surface the tizen80 TFM compiles against: read the bytes.
        private static string ReadUtf8(IntPtr p)
        {
            int n = 0;
            while (Marshal.ReadByte(p, n) != 0) n++;
            var bytes = new byte[n];
            Marshal.Copy(p, bytes, 0, n);
            return Encoding.UTF8.GetString(bytes);
        }

        private int Open(uint epoch, string uri, int x, int y, int w, int h)
        {
            if (!EnsureThreads()) return Native.PLX_HOST_PLAYER_E_UNAVAILABLE;
            Interlocked.Exchange(ref latestEpoch, epoch);
            return TryEnqueue(() => DoOpen(epoch, uri, x, y, w, h));
        }

        private int Verb(uint epoch, uint verb, long arg0, long arg1)
        {
            if (!EnsureThreads()) return Native.PLX_HOST_PLAYER_E_UNAVAILABLE;
            switch ((int)verb)
            {
                case Native.PLX_HOST_PLAYER_VERB_CLOSE:
                    // Always accepted: it ends in one PLAYER_CLOSED whether or not a player exists.
                    return TryEnqueue(() => DoClose(epoch));
                case Native.PLX_HOST_PLAYER_VERB_PLAY:
                case Native.PLX_HOST_PLAYER_VERB_PAUSE:
                case Native.PLX_HOST_PLAYER_VERB_SEEK:
                    if (Interlocked.Read(ref latestEpoch) != epoch) return Native.PLX_HOST_PLAYER_E_STALE;
                    return TryEnqueue(() => DoVerb(epoch, (int)verb, arg0, arg1));
                default:
                    return Native.PLX_HOST_PLAYER_E_ARGS;
            }
        }

        private int TryEnqueue(Action job)
        {
            try
            {
                return queue.TryAdd(job) ? Native.PLX_HOST_PLAYER_OK : Native.PLX_HOST_PLAYER_E_BUSY;
            }
            catch (InvalidOperationException)
            {
                return Native.PLX_HOST_PLAYER_E_UNAVAILABLE; // shut down
            }
        }

        // A native callback's hand-off to the video thread (it never blocks the callback thread: a full queue drops the job and says so).
        private void Enqueue(uint epoch, Action<Session> job)
        {
            if (TryEnqueue(() =>
            {
                Session s = current;
                if (s != null && s.Epoch == epoch) job(s);
            }) != Native.PLX_HOST_PLAYER_OK)
            {
                PlxApp.Log("player: a callback for epoch " + epoch + " was dropped (the request queue is full or shut down)");
            }
        }

        private bool EnsureThreads()
        {
            if (threadsFailed) return false;
            lock (startGate)
            {
                if (videoThread != null) return true;
                try
                {
                    videoThread = new Thread(VideoLoop) { IsBackground = true, Name = "plx-player" };
                    pollThread = new Thread(PollLoop) { IsBackground = true, Name = "plx-player-poll" };
                    videoThread.Start();
                    pollThread.Start();
                    return true;
                }
                catch (Exception e)
                {
                    threadsFailed = true;
                    PlxApp.Log("player: the video thread could not be started: " + e.GetType().Name + ": " + e.Message);
                    return false;
                }
            }
        }

        // ---- the video thread ---------------------------------------------------------------------------------------------------------------

        private void VideoLoop()
        {
            foreach (Action job in queue.GetConsumingEnumerable())
            {
                try { job(); }
                catch (Exception e) { PlxApp.Log("player: a request threw: " + e.GetType().Name + ": " + e.Message); }
            }
        }

        private static void Post(uint kind, uint epoch, long a, long b) { PlxApp.PostPlayer(kind, epoch, a, b); }

        private static void Fail(Session s, string step, int rc)
        {
            PlxApp.Log("player: epoch " + s.Epoch + ": " + step + " returned " + rc);
            Post((uint)Native.PLX_HOST_EVENT_PLAYER_ERROR, s.Epoch, rc, 0);
        }

        private void DoOpen(uint epoch, string uri, int x, int y, int w, int h)
        {
            if (current != null) DoClose(current.Epoch); // an open while another is open closes that one first
            var s = new Session(epoch);
            current = s;
            published = s;
            IntPtr user = UserOf(epoch);
            int rc = MediaPlayerNative.player_create(out s.Handle);
            if (rc != 0 || s.Handle == IntPtr.Zero) { s.Handle = IntPtr.Zero; Fail(s, "player_create", rc); return; }
            rc = MediaPlayerNative.player_set_uri(s.Handle, uri);
            if (rc != 0) { Fail(s, "player_set_uri", rc); return; }
            // The display is bound BEFORE prepare: measured on the emulator, a player with no display does not advance.
            rc = BindDisplay(s, x, y, w, h);
            if (rc != 0) { Fail(s, "bind the display", rc); return; }
            rc = MediaPlayerNative.player_set_display_mode(s.Handle, MediaPlayerNative.DISPLAY_MODE_LETTER_BOX);
            if (rc != 0) { Fail(s, "player_set_display_mode", rc); return; }
            MediaPlayerNative.player_set_completed_cb(s.Handle, CompletedCb, user);
            MediaPlayerNative.player_set_error_cb(s.Handle, ErrorCb, user);
            MediaPlayerNative.player_set_buffering_cb(s.Handle, BufferingCb, user);
            MediaPlayerNative.player_set_interrupted_cb(s.Handle, InterruptedCb, user);
            rc = MediaPlayerNative.player_prepare_async(s.Handle, PreparedCb, user);
            if (rc != 0) { Fail(s, "player_prepare_async", rc); return; }
        }

        // Find the library's window and bind the player to it as an OVERLAY. Ecore belongs to the main thread (plx_host.h, THREADS), so the two
        // Ecore calls and the bind run there, and this thread waits for them with a bound. UNVERIFIED which thread the native library needs.
        private int BindDisplay(Session s, int x, int y, int w, int h)
        {
            int rc = -1;
            bool ran = PlxApp.RunOnMain(() =>
            {
                lock (s.HandleGate)
                {
                    if (s.Closing || s.Handle == IntPtr.Zero) return;
                    IntPtr display = MediaPlayerNative.ecore_wl2_connected_display_get(IntPtr.Zero);
                    IntPtr window = display == IntPtr.Zero ? IntPtr.Zero : MediaPlayerNative.ecore_wl2_display_window_find(display, LibraryWindowId);
                    PlxApp.Log("player: ecore window id " + LibraryWindowId + " -> 0x" + window.ToInt64().ToString("x")
                        + " (compare with the library's `ecore: window_new(...) -> ` line)");
                    if (window == IntPtr.Zero) { rc = -2; return; }
                    rc = MediaPlayerNative.player_set_ecore_wl_display(s.Handle, MediaPlayerNative.DISPLAY_TYPE_OVERLAY, window, x, y, w, h);
                }
            }, BindWaitMs);
            if (!ran)
            {
                PlxApp.Log("player: binding the display did not finish within " + BindWaitMs + " ms on the main thread");
                return -3;
            }
            return rc;
        }

        private void DoVerb(uint epoch, int verb, long arg0, long arg1)
        {
            Session s = current;
            if (s == null || s.Epoch != epoch || s.Handle == IntPtr.Zero || s.Closing) return;
            switch (verb)
            {
                case Native.PLX_HOST_PLAYER_VERB_PLAY: DoPlay(s); break;
                case Native.PLX_HOST_PLAYER_VERB_PAUSE: DoPause(s); break;
                case Native.PLX_HOST_PLAYER_VERB_SEEK: DoSeek(s, arg0, arg1); break;
            }
        }

        private void DoPlay(Session s)
        {
            if (!s.Prepared) { s.PlayPending = true; return; } // starts at PREPARED
            s.PlayPending = false;
            int rc = MediaPlayerNative.player_start(s.Handle);
            if (rc != 0) { Fail(s, "player_start", rc); return; }
            s.Playing = true;
            Post((uint)Native.PLX_HOST_EVENT_PLAYER_STARTED, s.Epoch, 0, 0);
        }

        private void DoPause(Session s)
        {
            s.PlayPending = false;
            if (!s.Prepared) return;
            int rc = MediaPlayerNative.player_pause(s.Handle);
            if (rc != 0) { Fail(s, "player_pause", rc); return; }
            s.Playing = false;
            Post((uint)Native.PLX_HOST_EVENT_PLAYER_PAUSED, s.Epoch, 0, 0);
        }

        private void DoSeek(Session s, long targetMs, long command)
        {
            if (!s.Prepared) { Fail(s, "seek before prepared", -1); return; }
            int ms = (int)Math.Max(0, Math.Min(targetMs, int.MaxValue));
            s.Seeks.Add(new[] { command, ms });
            int rc = MediaPlayerNative.player_set_play_position(s.Handle, ms, 1, SeekDoneCb, UserOf(s.Epoch));
            if (rc != 0)
            {
                s.Seeks.RemoveAt(s.Seeks.Count - 1);
                Fail(s, "player_set_play_position", rc);
            }
        }

        private void OnPrepared(Session s)
        {
            s.Prepared = true;
            int duration;
            int rc = MediaPlayerNative.player_get_duration(s.Handle, out duration);
            Post((uint)Native.PLX_HOST_EVENT_PLAYER_PREPARED, s.Epoch, rc == 0 ? duration : 0, 0);
            int width, height;
            if (MediaPlayerNative.player_get_video_size(s.Handle, out width, out height) == 0 && width > 0 && height > 0)
            {
                Post((uint)Native.PLX_HOST_EVENT_PLAYER_VIDEO_SIZE, s.Epoch, width, height);
            }
            if (s.PlayPending) DoPlay(s);
        }

        private void OnSeekDone(Session s)
        {
            if (s.Seeks.Count == 0) return;
            long[] head = s.Seeks[0];
            s.Seeks.RemoveAt(0);
            Post((uint)Native.PLX_HOST_EVENT_PLAYER_SEEK_DONE, s.Epoch, head[0], head[1]);
        }

        // MEASURED: the completed callback fires with the native state still PLAYING (it changes at player_stop), so the state cannot validate
        // it. The callback is reported as it fired; the poller stops reading the position.
        private void OnCompleted(Session s)
        {
            s.Playing = false;
            Post((uint)Native.PLX_HOST_EVENT_PLAYER_COMPLETED, s.Epoch, 0, 0);
        }

        // CLOSE: stop, unprepare and destroy (once), then exactly one PLAYER_CLOSED for the epoch.
        private void DoClose(uint epoch)
        {
            Session s = current;
            if (s != null && s.Epoch == epoch)
            {
                Release(s);
                current = null;
                published = null;
            }
            Post((uint)Native.PLX_HOST_EVENT_PLAYER_CLOSED, epoch, 0, 0);
        }

        private static void Release(Session s)
        {
            s.Closing = true;
            s.Playing = false;
            if (s.Handle == IntPtr.Zero) return;
            // A position read or the display bind may be inside the handle: wait for it, with a bound. A read that never returns leaks the handle
            // (the process exit is its teardown) rather than destroying it under the read.
            if (!Monitor.TryEnter(s.HandleGate, CloseWaitMs))
            {
                PlxApp.Log("player: epoch " + s.Epoch + ": a native call is stuck on the handle; it is left undestroyed");
                return;
            }
            try
            {
                // stop and unprepare may answer an invalid-state code for a player that was never started or prepared: that is expected here.
                MediaPlayerNative.player_stop(s.Handle);
                MediaPlayerNative.player_unprepare(s.Handle);
                int rc = MediaPlayerNative.player_destroy(s.Handle);
                if (rc != 0) PlxApp.Log("player: epoch " + s.Epoch + ": player_destroy returned " + rc);
                s.Handle = IntPtr.Zero;
            }
            finally
            {
                Monitor.Exit(s.HandleGate);
            }
        }

        // ---- the position poller ------------------------------------------------------------------------------------------------------------

        private void PollLoop()
        {
            while (!shuttingDown)
            {
                Thread.Sleep(PollMs);
                Session s = published;
                if (s == null || !s.Playing || s.Closing) continue;
                try { PollOnce(s); }
                catch (Exception e) { PlxApp.Log("player: position poll: " + e.GetType().Name + ": " + e.Message); }
            }
        }

        private void PollOnce(Session s)
        {
            Task<int> read = Task.Run(() => ReadPosition(s));
            if (!read.Wait(StallMs))
            {
                if (!s.StallSent)
                {
                    s.StallSent = true;
                    Post((uint)Native.PLX_HOST_EVENT_PLAYER_STALL, s.Epoch, 0, 0);
                }
                while (!read.Wait(500))
                {
                    if (shuttingDown) return;
                }
            }
            int ms = read.Result;
            if (ms >= 0)
            {
                s.StallSent = false;
                if (!s.Closing) Post((uint)Native.PLX_HOST_EVENT_PLAYER_POSITION, s.Epoch, ms, 0);
            }
        }

        private static int ReadPosition(Session s)
        {
            if (!Monitor.TryEnter(s.HandleGate)) return -1;
            try
            {
                if (s.Closing || s.Handle == IntPtr.Zero) return -1;
                int ms;
                return MediaPlayerNative.player_get_play_position(s.Handle, out ms) == 0 ? ms : -1;
            }
            finally
            {
                Monitor.Exit(s.HandleGate);
            }
        }

        // ---- the end ------------------------------------------------------------------------------------------------------------------------

        // OnTerminate, after the library was asked to stop: close any player that is still open, with a bound, and end the threads. Requests
        // after this are refused as unavailable. The library's own stop already refuses requests, so this only catches a player the library
        // could not close itself (a host-initiated stop).
        internal void Shutdown()
        {
            shuttingDown = true;
            Thread video;
            lock (startGate) { video = videoThread; }
            if (video == null) return;
            try
            {
                queue.TryAdd(() =>
                {
                    Session s = current;
                    if (s != null) DoClose(s.Epoch);
                });
                queue.CompleteAdding();
            }
            catch (InvalidOperationException) { }
            if (!video.Join(CloseWaitMs + 1000)) PlxApp.Log("player: the video thread did not end within the bound; the process exit is the teardown");
        }
    }
}
