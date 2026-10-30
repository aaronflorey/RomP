"""Offline checks: python3 -m unittest discover -s packaging/steamos."""
import hashlib
import io
import json
from pathlib import Path
import ctypes
import ctypes.util
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import install


def desktop_args(command):
    # GLib applies desktop Exec quoting after the desktop string unescape pass.
    glib = ctypes.CDLL(ctypes.util.find_library('glib-2.0'))
    argc = ctypes.c_int()
    argv = ctypes.POINTER(ctypes.c_char_p)()
    success = glib.g_shell_parse_argv(command.replace('\\\\', '\\').encode(),
                                      ctypes.byref(argc), ctypes.byref(argv), None)
    if not success:
        raise ValueError('Invalid desktop command')
    try:
        return [argv[i].decode() for i in range(argc.value)]
    finally:
        glib.g_strfreev(argv)


class InstallerTests(unittest.TestCase):
    def run_install(self, home, corrupt=False, add=True):
        name = 'RomP-1.0-x86_64.AppImage'
        url = install.REPO + '/releases/download/v1.0/'
        release = {'assets': [{'name': n, 'browser_download_url': url + n}
                              for n in (name, name + '.sha512')]}
        payload = b'appimage fixture'

        def download(source, dest):
            if source.endswith('.sha512'):
                digest = 'bad' if corrupt else hashlib.sha512(payload).hexdigest()
                dest.write_text(digest + '  ' + name)
            else:
                dest.write_bytes(payload)

        def run(args, **kwargs):
            return subprocess.CompletedProcess(args, 0 if add or '--yesno' not in args else 1)

        with patch.object(install.platform, 'system', return_value='Linux'), \
             patch.object(install.platform, 'machine', return_value='x86_64'), \
             patch.object(install.os, 'geteuid', return_value=1000), \
             patch.object(install.shutil, 'which', return_value='/usr/bin/steamos-add-to-steam'), \
             patch.object(install.Path, 'home', return_value=home), \
             patch.dict(install.os.environ, {'XDG_DATA_HOME': str(home / 'data')}), \
             patch.object(install.urllib.request, 'urlopen', return_value=io.BytesIO(json.dumps(release).encode())), \
             patch.object(install, 'download', side_effect=download), \
             patch.object(install.subprocess, 'run', side_effect=run) as calls:
            install.install()
            return calls

    def test_install_and_update_without_duplicate(self):
        with tempfile.TemporaryDirectory() as temp:
            home = Path(temp) / 'home with spaces'
            calls = self.run_install(home)
            image = home / 'Applications/RomP/RomP.AppImage'
            self.assertEqual(image.read_bytes(), b'appimage fixture')
            self.assertTrue(image.stat().st_mode & 0o111)
            launcher = home / 'data/applications/io.github.rompemu.romp.desktop'
            self.assertIn('--appimage-extract-and-run', launcher.read_text())
            self.assertEqual(calls.call_args.args[0], ['/usr/bin/steamos-add-to-steam', str(launcher)])
            calls = self.run_install(home, add=False)
            self.assertTrue(all(c.args[0][0] == 'kdialog' for c in calls.call_args_list))

    def test_checksum_failure_preserves_old_app(self):
        with tempfile.TemporaryDirectory() as temp:
            home = Path(temp)
            image = home / 'Applications/RomP/RomP.AppImage'
            image.parent.mkdir(parents=True)
            image.write_bytes(b'old app')
            with self.assertRaisesRegex(RuntimeError, 'checksum'):
                self.run_install(home, corrupt=True)
            self.assertEqual(image.read_bytes(), b'old app')
            self.assertEqual(list(image.parent.iterdir()), [image])

    def test_unexpected_download_host_rejected(self):
        with self.assertRaises(ValueError):
            install.download('https://example.com/app', Path('/unused'))

    def test_desktop_quoting_and_bootstrap_syntax(self):
        value = '/home/a b/$`"\\/RomP.AppImage'
        self.assertEqual(desktop_args(install.desktop_quote(value)), [value])
        desktop = Path(__file__).with_name('Install-RomP.desktop').read_text()
        command = next(line[5:] for line in desktop.splitlines() if line.startswith('Exec='))
        args = desktop_args(command)
        self.assertEqual(args[:2], ['bash', '-c'])
        subprocess.run(['bash', '-n', '-c', args[2]], check=True)
