#!/usr/bin/env python3
"""Unit tests of tizen/package.py: input validation, the staging contract, the signing refusal and the package check.

Nothing here runs dotnet or needs a certificate: `stage` and `verify_tpk` work on a throwaway tree of fake files, and the signing refusal is
the part a CI machine can prove. The refusal must NAME the missing variables and must never print a value.
"""
import importlib.util
import json
import tempfile
import unittest
import zipfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("tizen_package", HERE.parent / "tizen" / "package.py")
package = importlib.util.module_from_spec(spec)
spec.loader.exec_module(package)

rspec = importlib.util.spec_from_file_location("tizen_log_receiver", HERE.parent / "tools" / "tizen-log-receiver.py")
receiver = importlib.util.module_from_spec(rspec)
rspec.loader.exec_module(receiver)

SECRET = "hunter2-do-not-print"


def fake_root(tmp: Path) -> Path:
    root = tmp / "root"
    for source, _ in package.PAYLOAD:
        (root / source).parent.mkdir(parents=True, exist_ok=True)
        (root / source).write_bytes(b"payload " + source.encode())
    (root / package.HOST_DIR).mkdir(parents=True)
    return root


class Inputs(unittest.TestCase):
    def test_log_host_is_an_ipv4_literal_and_port(self):
        self.assertEqual(package.log_host("10.0.2.2:9999"), "10.0.2.2:9999")
        for bad in ("localhost:9999", "10.0.2.2", "10.0.2.256:1", "10.0.2.2:0", "10.0.2.2:70000", "http://10.0.2.2:9", "10.0.2.2:9 "):
            with self.assertRaises(package.PackageError, msg=bad):
                package.log_host(bad)

    def test_mock_host_is_host_and_port_never_a_url(self):
        self.assertEqual(package.mock_host("10.0.2.2:32499"), "10.0.2.2:32499")
        self.assertEqual(package.mock_host("mock.lan:8080"), "mock.lan:8080")
        for bad in ("http://10.0.2.2:1", "10.0.2.2", "10.0.2.2:99999", "300.0.0.1:5", "-bad.host:5", "a..b:5", "10.0.2.2:5/x"):
            with self.assertRaises(package.PackageError, msg=bad):
                package.mock_host(bad)

    def test_dev_boot_carries_a_placeholder_only(self):
        text = package.dev_boot_json("10.0.2.2:32499")
        parsed = json.loads(text)
        self.assertEqual((parsed["pms_host"], parsed["pms_port"]), ("10.0.2.2", 32499))
        self.assertEqual(parsed["dev"], {"token": package.DEV_BOOT_TOKEN, "pms_origin": "http://10.0.2.2:32499"})
        self.assertIn("placeholder", package.DEV_BOOT_TOKEN)

    def test_msbuild_escape_percent_first(self):
        self.assertEqual(package.msbuild_escape("a%41;b@(x)"), "a%2541%3Bb%40%28x%29")


class Signing(unittest.TestCase):
    def test_refusal_names_the_missing_variables_and_no_value(self):
        env = {"PLX_TIZEN_AUTHOR_PASS": SECRET, "PLX_TIZEN_DIST_P12": "/nonexistent/dist.p12"}
        with self.assertRaises(package.PackageError) as caught:
            package.check_signing(env)
        text = str(caught.exception)
        self.assertIn("PLX_TIZEN_AUTHOR_P12", text)
        self.assertIn("PLX_TIZEN_DIST_PASS", text)
        self.assertNotIn(SECRET, text)
        self.assertNotIn("/nonexistent", text)

    def test_nothing_set_refuses_all_four(self):
        self.assertEqual(package.missing_signing({}), list(package.SIGNING_VARS))

    def test_a_path_that_is_no_file_is_named_without_its_value(self):
        env = {name: "/nonexistent/x" for name in package.SIGNING_VARS}
        with self.assertRaises(package.PackageError) as caught:
            package.check_signing(env)
        self.assertIn("PLX_TIZEN_AUTHOR_P12 names no file", str(caught.exception))
        self.assertNotIn("/nonexistent", str(caught.exception))

    def test_complete_inputs_resolve_paths(self):
        with tempfile.TemporaryDirectory() as tmp:
            a, d = Path(tmp) / "a.p12", Path(tmp) / "d.p12"
            a.write_bytes(b"x")
            d.write_bytes(b"x")
            env = {"PLX_TIZEN_AUTHOR_P12": str(a), "PLX_TIZEN_AUTHOR_PASS": "p", "PLX_TIZEN_DIST_P12": str(d), "PLX_TIZEN_DIST_PASS": "q"}
            self.assertEqual(package.check_signing(env), (a.resolve(), d.resolve()))

    def test_the_tpk_build_refuses_before_staging_anything(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = fake_root(Path(tmp))
            so = Path(tmp) / "libplx.so"
            so.write_bytes(b"elf")
            with self.assertRaises(package.PackageError):
                package.build_tpk(root, so, None, None, {})
            self.assertFalse((root / package.HOST_DIR / "lib").exists())


class Staging(unittest.TestCase):
    def test_stage_writes_the_payload_and_the_asked_for_files_only(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = fake_root(Path(tmp))
            so = Path(tmp) / "libplx.so"
            so.write_bytes(b"elf")
            host = package.stage(root, so, None, None)
            self.assertEqual((host / "lib/libplx.so").read_bytes(), b"elf")
            self.assertTrue((host / "res/appfont.ttf").is_file())
            self.assertFalse((host / "res/dev-boot.json").exists())
            self.assertFalse((host / "res/log-host.txt").exists())
            host = package.stage(root, so, "10.0.2.2:32499", "10.0.2.2:9999")
            self.assertTrue((host / "res/dev-boot.json").is_file())
            self.assertEqual((host / "res/log-host.txt").read_text(), "10.0.2.2:9999\n")
            host = package.stage(root, so, None, None)  # a stale dev-boot of the last staging must not ride along
            self.assertFalse((host / "res/dev-boot.json").exists())
            self.assertFalse((host / "res/log-host.txt").exists())

    def test_stage_refuses_a_missing_library(self):
        with tempfile.TemporaryDirectory() as tmp:
            with self.assertRaises(package.PackageError):
                package.stage(fake_root(Path(tmp)), Path(tmp) / "none.so", None, None)


class PackageCheck(unittest.TestCase):
    def make(self, tmp: Path, names, so_bytes=b"elf") -> tuple[Path, Path]:
        so = tmp / "libplx.so"
        so.write_bytes(b"elf")
        tpk = tmp / "p.tpk"
        with zipfile.ZipFile(tpk, "w") as z:
            for name in names:
                z.writestr(name, so_bytes if name == "lib/libplx.so" else "x")
        return tpk, so

    def test_complete_package_passes(self):
        with tempfile.TemporaryDirectory() as tmp:
            tpk, so = self.make(Path(tmp), package.REQUIRED_IN_TPK)
            package.verify_tpk(tpk, so, None, None)

    def test_missing_member_is_named(self):
        with tempfile.TemporaryDirectory() as tmp:
            tpk, so = self.make(Path(tmp), [n for n in package.REQUIRED_IN_TPK if n != "res/OFL.txt"])
            with self.assertRaisesRegex(package.PackageError, "OFL.txt"):
                package.verify_tpk(tpk, so, None, None)

    def test_a_stale_dev_boot_is_refused(self):
        with tempfile.TemporaryDirectory() as tmp:
            tpk, so = self.make(Path(tmp), (*package.REQUIRED_IN_TPK, "res/dev-boot.json"))
            with self.assertRaisesRegex(package.PackageError, "carries res/dev-boot.json"):
                package.verify_tpk(tpk, so, None, None)

    def test_a_requested_log_file_must_be_there(self):
        with tempfile.TemporaryDirectory() as tmp:
            tpk, so = self.make(Path(tmp), package.REQUIRED_IN_TPK)
            with self.assertRaisesRegex(package.PackageError, "lacks res/log-host.txt"):
                package.verify_tpk(tpk, so, None, "10.0.2.2:9")

    def test_packaged_library_must_be_the_verified_one(self):
        with tempfile.TemporaryDirectory() as tmp:
            tpk, so = self.make(Path(tmp), package.REQUIRED_IN_TPK, so_bytes=b"other")
            with self.assertRaisesRegex(package.PackageError, "differs"):
                package.verify_tpk(tpk, so, None, None)


class LogReceiver(unittest.TestCase):
    """tools/tizen-log-receiver.py's sequence bookkeeping: a gap is the only evidence a UDP log is incomplete."""

    def test_in_order_lines_report_nothing(self):
        tracker = receiver.Tracker()
        self.assertEqual([tracker.note("a", f"{n} line") for n in range(3)], [None, None, None])
        self.assertEqual(tracker.lost, 0)

    def test_a_gap_counts_the_lost_datagrams(self):
        tracker = receiver.Tracker()
        tracker.note("a", "0 x")
        report = tracker.note("a", "4 x")
        self.assertIn("3 datagram(s) lost", report)
        self.assertEqual(tracker.lost, 3)

    def test_the_host_stream_and_the_library_stream_count_apart(self):
        tracker = receiver.Tracker()
        self.assertIsNone(tracker.note("a", "0 lib"))
        self.assertIsNone(tracker.note("a", "h0 host"))
        self.assertIsNone(tracker.note("a", "h1 host"))
        self.assertIsNone(tracker.note("a", "1 lib"))

    def test_a_restart_is_not_a_loss(self):
        tracker = receiver.Tracker()
        tracker.note("a", "0 x")
        tracker.note("a", "1 x")
        self.assertIn("restarted", tracker.note("a", "0 x"))
        self.assertEqual((tracker.lost, tracker.restarts), (0, 1))


if __name__ == "__main__":
    unittest.main()
