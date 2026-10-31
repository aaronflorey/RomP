#!/usr/bin/env python3
"""Install the nightly RomP AppImage without modifying SteamOS or Steam's files."""

import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import tempfile
import urllib.request


REPO = "https://github.com/RompEmu/RomP"


def download(url, target):
    if not url.startswith(REPO + "/releases/download/"):
        raise ValueError("Unexpected release download URL")
    with urllib.request.urlopen(url, timeout=60) as source, target.open("wb") as dest:
        shutil.copyfileobj(source, dest)


def desktop_quote(value):
    # Desktop Exec has its own quoting rules, independent of shell quoting.
    if any(c in value for c in "\n\r\0"):
        raise ValueError("Unsupported character in installation path")
    quoted = ''.join('\\' + c if c in '\\"`$' else c for c in value)
    return '"' + quoted.replace('\\', '\\\\') + '"'


def install():
    if platform.system() != "Linux" or platform.machine() != "x86_64":
        raise RuntimeError("This installer needs x86_64 Linux (SteamOS).")
    if os.geteuid() == 0:
        raise RuntimeError("Run this installer as your normal user, without sudo.")
    helper = shutil.which("steamos-add-to-steam")
    if not helper:
        raise RuntimeError("SteamOS's steamos-add-to-steam helper was not found.")

    subprocess.run(["kdialog", "--title", "Install RomP", "--msgbox",
                    "RomP will download the latest nightly build to ~/Applications/RomP. "
                    "An existing installation there will be updated. Close RomP before continuing. "
                    "No password is needed. The download may take a few minutes."], check=True)
    request = urllib.request.Request(
        "https://api.github.com/repos/RompEmu/RomP/releases/tags/nightly",
        headers={"Accept": "application/vnd.github+json", "User-Agent": "RomP-installer"},
    )
    with urllib.request.urlopen(request, timeout=60) as response:
        release = json.load(response)
    assets = {a["name"]: a["browser_download_url"] for a in release["assets"]}
    images = [name for name in assets if name.endswith("-x86_64.AppImage")]
    if len(images) != 1 or images[0] + ".sha512" not in assets:
        raise RuntimeError("The nightly release has no x86_64 AppImage and checksum.")
    name = images[0]
    folder = Path.home() / "Applications" / "RomP"
    folder.mkdir(parents=True, exist_ok=True)
    # Stage on the same filesystem, so a failed download leaves the old app intact.
    with tempfile.TemporaryDirectory(prefix=".install-", dir=folder) as staging:
        image = Path(staging) / name
        checksum = Path(staging) / "checksum"
        download(assets[name], image)
        download(assets[name + ".sha512"], checksum)
        expected = checksum.read_text().split()[0]
        with image.open("rb") as source:
            digest = hashlib.sha512()
            for block in iter(lambda: source.read(1024 * 1024), b""):
                digest.update(block)
        if digest.hexdigest() != expected:
            raise RuntimeError("Download checksum failed; your existing app was kept.")
        image.chmod(0o755)
        image.replace(folder / "RomP.AppImage")

    applications = Path(os.environ.get("XDG_DATA_HOME", Path.home() / ".local/share")) / "applications"
    applications.mkdir(parents=True, exist_ok=True)
    launcher = applications / "io.github.rompemu.romp.desktop"
    # Percent is a desktop field-code marker even inside quotes.
    executable = desktop_quote(str(folder / "RomP.AppImage").replace("%", "%%"))
    launcher.write_text(
        "[Desktop Entry]\nType=Application\nName=RomP\n"
        "Comment=Play retro games from your RomM library\n"
        f"Exec={executable} --appimage-extract-and-run\n"
        "Icon=applications-games\nCategories=Game;Emulator;\nTerminal=false\n"
    )
    launcher.chmod(0o755)
    add = subprocess.run(["kdialog", "--title", "RomP installed", "--yesno",
                          "Add RomP to Steam now? If RomP is already in Steam, choose No "
                          "to avoid a duplicate. You can then return to Gaming Mode and "
                          "launch RomP. Use Steam + X when you need the on-screen keyboard."])
    if add.returncode == 0:
        subprocess.run([helper, str(launcher)], check=True)
    elif add.returncode != 1:
        raise RuntimeError("Could not display the Add to Steam prompt.")


if __name__ == "__main__":
    try:
        install()
    except Exception as error:
        message = f"RomP installation could not finish: {error}\nYou can run the installer again."
        print(message, flush=True)
        subprocess.run(["kdialog", "--title", "Install RomP", "--error", message])
        raise SystemExit(1)
