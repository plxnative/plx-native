// PROBE ONLY (branch claude/tizen-subtitle-probe; never shipped): does the device's player hand the app subtitle cue text, and from which
// source (an embedded text track chosen with player_select_track(TEXT), or a sidecar file given to player_set_subtitle_path)? Inert unless
// the file <app data>/subprobe.txt exists; it is read once per open, `key=value` per line:
//   cb=1            register player_set_subtitle_updated_cb before prepare
//   path=<p>        call player_set_subtitle_path(p) before prepare (a local path or an http URL)
//   select=<n>      at PREPARED, player_select_track(TEXT, n); -1 (the default) selects nothing
//   offset=<ms>     at the first cue, player_set_subtitle_position_offset(ms)
//
// The contract, from the Tizen 3.0 rootstrap's usr/include/media/player.h and TizenFX API8 Interop.Player.cs:
//   typedef void (*player_subtitle_updated_cb)(unsigned long duration, char *text, void *user_data);   (TizenFX: uint, string)
//   int player_set_subtitle_updated_cb(player_h, player_subtitle_updated_cb, void *);  "@pre The subtitle must be set by
//       player_set_subtitle_path()"; the callback runs on a separate thread, not the main loop.
//   int player_set_subtitle_path(player_h, const char *path);  .sub/.smi/.srt; IDLE/READY/PLAYING/PAUSED.
//   int player_set_subtitle_position_offset(player_h, int millisecond);  PLAYING or PAUSED, path set.
//   PLAYER_STREAM_TYPE_TEXT = 3.
// `unsigned long` is the pointer width on both LP64 x86_64 (the emulator) and ILP32 ARM, so the duration is a UIntPtr here.

using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;

namespace PlxHost
{
    internal static class SubtitleProbe
    {
        internal const int STREAM_TYPE_TEXT = 3;

        [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
        internal delegate void SubtitleCallback(UIntPtr duration, IntPtr text, IntPtr userData);

        [DllImport(MediaPlayerNative.MediaLib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern int player_set_subtitle_updated_cb(IntPtr player, SubtitleCallback cb, IntPtr userData);

        [DllImport(MediaPlayerNative.MediaLib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern int player_set_subtitle_path(IntPtr player, [MarshalAs(UnmanagedType.LPUTF8Str)] string path);

        [DllImport(MediaPlayerNative.MediaLib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern int player_set_subtitle_position_offset(IntPtr player, int millisecond);

        private static readonly SubtitleCallback Cb = OnCue; // rooted for the process
        private static IntPtr handle;
        private static readonly Stopwatch Clock = new Stopwatch();
        private static int cues;
        private static int offset;
        private static bool offsetSent;
        private static int select = -1;

        private static Dictionary<string, string> ReadConfig()
        {
            try
            {
                string data = Tizen.Applications.Application.Current.DirectoryInfo.Data;
                string file = Path.Combine(data, "subprobe.txt");
                if (!File.Exists(file)) return null;
                var d = new Dictionary<string, string>();
                foreach (string raw in File.ReadAllLines(file))
                {
                    string line = raw.Trim();
                    int eq = line.IndexOf('=');
                    if (eq > 0) d[line.Substring(0, eq).Trim()] = line.Substring(eq + 1).Trim();
                }
                return d;
            }
            catch (Exception e)
            {
                PlxApp.Log("subprobe: config read threw " + e.GetType().Name + ": " + e.Message);
                return null;
            }
        }

        // Before prepare (IDLE).
        internal static void BeforePrepare(IntPtr player)
        {
            var cfg = ReadConfig();
            handle = IntPtr.Zero;
            if (cfg == null) return;
            handle = player;
            cues = 0;
            offsetSent = false;
            string v;
            select = cfg.TryGetValue("select", out v) && int.TryParse(v, out int n) ? n : -1;
            offset = cfg.TryGetValue("offset", out v) && int.TryParse(v, out int o) ? o : 0;
            Clock.Restart();
            if (cfg.TryGetValue("path", out v) && v.Length > 0)
            {
                int rc = player_set_subtitle_path(player, v);
                PlxApp.Log("subprobe: player_set_subtitle_path(" + v + ") returned " + rc);
            }
            if (cfg.TryGetValue("cb", out v) && v == "1")
            {
                int rc = player_set_subtitle_updated_cb(player, Cb, IntPtr.Zero);
                PlxApp.Log("subprobe: player_set_subtitle_updated_cb returned " + rc);
            }
        }

        // At PREPARED (READY), on the video thread.
        internal static void AtPrepared(IntPtr player)
        {
            if (handle != player || handle == IntPtr.Zero) return;
            int count;
            int rc = MediaPlayerNative.player_get_track_count(player, STREAM_TYPE_TEXT, out count);
            int cur;
            int rcCur = MediaPlayerNative.player_get_current_track(player, STREAM_TYPE_TEXT, out cur);
            var langs = new List<string>();
            for (int i = 0; rc == 0 && i < count; i++)
            {
                IntPtr code;
                int rcl = MediaPlayerNative.player_get_track_language_code(player, STREAM_TYPE_TEXT, i, out code);
                string lang = code == IntPtr.Zero ? "(null)" : Marshal.PtrToStringUTF8(code);
                if (code != IntPtr.Zero) Marshal.FreeHGlobal(code);
                langs.Add(i + "=" + lang + (rcl != 0 ? "(rc " + rcl + ")" : ""));
            }
            PlxApp.Log("subprobe: at PREPARED +" + Clock.ElapsedMilliseconds + " ms: text track count rc=" + rc + " count=" + count
                + " current rc=" + rcCur + " idx=" + cur + " [" + string.Join(", ", langs) + "]");
            if (select >= 0)
            {
                int rcs = MediaPlayerNative.player_select_track(player, STREAM_TYPE_TEXT, select);
                int after;
                int rca = MediaPlayerNative.player_get_current_track(player, STREAM_TYPE_TEXT, out after);
                PlxApp.Log("subprobe: player_select_track(TEXT, " + select + ") returned " + rcs + "; current now rc=" + rca + " idx=" + after);
            }
        }

        // On the player's own thread.
        private static void OnCue(UIntPtr duration, IntPtr text, IntPtr userData)
        {
            try
            {
                IntPtr h = handle;
                int pos = -1;
                if (h != IntPtr.Zero) MediaPlayerNative.player_get_play_position(h, out pos);
                string s = text == IntPtr.Zero ? "(null)" : Marshal.PtrToStringUTF8(text);
                cues++;
                PlxApp.Log("subprobe: cue #" + cues + " +" + Clock.ElapsedMilliseconds + " ms pos=" + pos + " dur=" + duration.ToUInt64()
                    + " thread=" + Environment.CurrentManagedThreadId + " text=[" + (s ?? "").Replace("\n", "\\n") + "]");
                if (offset != 0 && !offsetSent && h != IntPtr.Zero)
                {
                    offsetSent = true;
                    int rc = player_set_subtitle_position_offset(h, offset);
                    PlxApp.Log("subprobe: player_set_subtitle_position_offset(" + offset + ") returned " + rc);
                }
            }
            catch (Exception e)
            {
                PlxApp.Log("subprobe: cue callback threw " + e.GetType().Name + ": " + e.Message);
            }
        }
    }
}
