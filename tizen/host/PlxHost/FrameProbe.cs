// PROBE ONLY (branch claude/tizen-frame-texture-probe; never shipped): can the device's player hand the app its decoded video frames,
// so the UI could draw the picture itself (and glass could stand over it)? Off by default: Mode stays Off and nothing below runs.
//
// The contract, from the Tizen 3.0 rootstrap's usr/include/media/player.h:
//   int player_set_media_packet_video_frame_decoded_cb(player_h, player_media_packet_video_decoded_cb, void *user_data);
//   typedef void (*player_media_packet_video_decoded_cb)(media_packet_h pkt, void *user_data);
//   @pre the player's state must be PLAYER_STATE_IDLE, and PLAYER_DISPLAY_TYPE_NONE must be set by player_set_display().
//   Called on a separate (gstreamer) thread; the packet must be media_packet_destroy()ed quickly, or the decoder runs out of buffers.
// media_packet.h / media_format.h (libcapi-media-tool) and tbm_surface.h (libtbm) give the frame's format, size and surface.

using System;
using System.Runtime.InteropServices;
using System.Threading;

namespace PlxHost
{
    internal static class FrameProbe
    {
        internal enum ProbeMode { Off, DisplayNone, KeepOverlay }

        // The probe script flips this in its build copy; the committed value keeps the shipping behaviour.
        internal const ProbeMode Mode = ProbeMode.Off;

        internal const string ToolLib = "libcapi-media-tool.so.0";
        internal const string TbmLib = "libtbm.so.1";
        internal const int PLAYER_DISPLAY_TYPE_NONE = 2;

        [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
        internal delegate void PacketCallback(IntPtr pkt, IntPtr userData);

        [DllImport(MediaPlayerNative.MediaLib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern int player_set_display(IntPtr player, int type, IntPtr display);

        [DllImport(MediaPlayerNative.MediaLib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern int player_set_media_packet_video_frame_decoded_cb(IntPtr player, PacketCallback cb, IntPtr userData);

        [DllImport(ToolLib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern int media_packet_get_format(IntPtr pkt, out IntPtr fmt);

        [DllImport(ToolLib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern int media_format_get_video_info(IntPtr fmt, out int mime, out int width, out int height, out int avgBps, out int maxBps);

        [DllImport(ToolLib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern int media_format_unref(IntPtr fmt);

        [DllImport(ToolLib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern int media_packet_get_tbm_surface(IntPtr pkt, out IntPtr surface);

        [DllImport(ToolLib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern int media_packet_get_pts(IntPtr pkt, out ulong pts);

        [DllImport(ToolLib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern int media_packet_destroy(IntPtr pkt);

        [DllImport(TbmLib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern uint tbm_surface_get_format(IntPtr surface);

        [DllImport(TbmLib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern int tbm_surface_get_width(IntPtr surface);

        [DllImport(TbmLib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern int tbm_surface_get_height(IntPtr surface);

        // Kept alive for the process: the native side holds the pointer.
        private static readonly PacketCallback Cb = OnPacket;
        private static long frames, inSecond, secondStartMs = Environment.TickCount64;

        // Step 2 hand-off: the newest tbm_surface and its packet, which the library's render thread may import (and then releases).
        private static IntPtr heldPacket = IntPtr.Zero, heldSurface = IntPtr.Zero;
        private static readonly object Gate = new object();

        private static string Fourcc(uint f) => new string(new[] { (char)(f & 0xff), (char)((f >> 8) & 0xff), (char)((f >> 16) & 0xff), (char)(f >> 24) });

        // Called from OpenPlayer instead of (DisplayNone) or after (KeepOverlay) the display bind; the player is still IDLE.
        internal static void Arm(IntPtr player)
        {
            if (Mode == ProbeMode.DisplayNone)
            {
                int d = player_set_display(player, PLAYER_DISPLAY_TYPE_NONE, IntPtr.Zero);
                PlxApp.Log("frameprobe: player_set_display(NONE) rc=" + d);
            }
            int rc = player_set_media_packet_video_frame_decoded_cb(player, Cb, IntPtr.Zero);
            PlxApp.Log("frameprobe: mode " + Mode + ": player_set_media_packet_video_frame_decoded_cb rc=" + rc);
        }

        private static void OnPacket(IntPtr pkt, IntPtr user)
        {
            long n = Interlocked.Increment(ref frames);
            try
            {
                if (n <= 3 || n % 300 == 0)
                {
                    int rf = media_packet_get_format(pkt, out IntPtr fmt);
                    int mime = 0, w = 0, h = 0, avg = 0, max = 0, rv = -1;
                    if (rf == 0 && fmt != IntPtr.Zero) { rv = media_format_get_video_info(fmt, out mime, out w, out h, out avg, out max); media_format_unref(fmt); }
                    int rs = media_packet_get_tbm_surface(pkt, out IntPtr surf);
                    media_packet_get_pts(pkt, out ulong pts);
                    string tbm = surf == IntPtr.Zero ? "none"
                        : Fourcc(tbm_surface_get_format(surf)) + " " + tbm_surface_get_width(surf) + "x" + tbm_surface_get_height(surf);
                    PlxApp.Log("frameprobe: frame " + n + " get_format rc=" + rf + " video_info rc=" + rv + " mime=0x" + mime.ToString("x")
                        + " " + w + "x" + h + "; get_tbm_surface rc=" + rs + " surface=" + (surf == IntPtr.Zero ? "0" : "set") + " tbm=" + tbm
                        + " pts=" + pts);
                }
                long now = Environment.TickCount64;
                inSecond++;
                if (now - secondStartMs >= 1000)
                {
                    PlxApp.Log("frameprobe: " + inSecond + " frames in " + (now - secondStartMs) + " ms (total " + n + ")");
                    inSecond = 0; secondStartMs = now;
                }
            }
            catch (Exception e)
            {
                if (n <= 3) PlxApp.Log("frameprobe: frame " + n + " threw " + e.GetType().Name + ": " + e.Message);
            }
            // Step 2: offer the newest surface to the library; the previous offer that nobody took is released here.
            IntPtr release = IntPtr.Zero;
            if (media_packet_get_tbm_surface(pkt, out IntPtr s2) == 0 && s2 != IntPtr.Zero)
            {
                lock (Gate) { release = heldPacket; heldPacket = pkt; heldSurface = s2; }
            }
            else release = pkt;
            if (release != IntPtr.Zero) media_packet_destroy(release);
        }

        // The library takes the newest surface (it imports it and calls Release when done with it).
        internal static IntPtr Take(out IntPtr packet)
        {
            lock (Gate) { packet = heldPacket; IntPtr s = heldSurface; heldPacket = IntPtr.Zero; heldSurface = IntPtr.Zero; return s; }
        }

        internal static void Release(IntPtr packet)
        {
            if (packet != IntPtr.Zero) media_packet_destroy(packet);
        }

        // All packets must be destroyed before player_unprepare (player.h).
        internal static void Drain()
        {
            IntPtr p; lock (Gate) { p = heldPacket; heldPacket = IntPtr.Zero; heldSurface = IntPtr.Zero; }
            if (p != IntPtr.Zero) media_packet_destroy(p);
        }
    }
}
