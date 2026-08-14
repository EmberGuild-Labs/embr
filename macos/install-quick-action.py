#!/usr/bin/env python3
"""Install a Finder Quick Action: right-click a file or folder -> Compress with EMBR.

Finder's built-in "Compress" item is not extensible — third parties cannot add
entries beside it. What they *can* add is a Service, which macOS surfaces in the
right-click menu under "Quick Actions". That is what this installs.

A Quick Action is an Automator .workflow bundle in ~/Library/Services:

    Compress with EMBR.workflow/
      Contents/Info.plist        <- declares the Service and its menu title
      Contents/document.wflow    <- the workflow itself: one Run Shell Script

Both are property lists, so they are built here with plistlib rather than
written out as XML by hand — the shell script embedded in document.wflow is
full of quotes and dollar signs, and hand-escaping it into XML is a good way to
produce a file that installs fine and silently does nothing.

    python3 macos/install-quick-action.py
"""

from __future__ import annotations

import plistlib
import shutil
import subprocess
import sys
import uuid
from pathlib import Path

SERVICES = Path.home() / "Library" / "Services"
NAME = "Compress with EMBR"
WORKFLOW = SERVICES / f"{NAME}.workflow"

# The script Finder runs. Paths of the selected items arrive as "$@".
#
# Mirrors what Finder's own Compress does: one selected item becomes
# "<name>.embr" beside it, several become "Archive.embr" in the same folder.
# Never overwrites — falls back to "name 2.embr", "name 3.embr", ...
SCRIPT = r'''
# Prefer the copy inside the app bundle so the Quick Action keeps working even
# if the CLI is not installed or not on PATH. Quick Actions do not get the PATH
# from your shell profile, so nothing here may be assumed to be on it.
EMBR="/Applications/EMBR.app/Contents/Resources/embr"
[ -x "$EMBR" ] || EMBR="$HOME/.cargo/bin/embr"
[ -x "$EMBR" ] || EMBR="/usr/local/bin/embr"
[ -x "$EMBR" ] || EMBR="/opt/homebrew/bin/embr"

if [ ! -x "$EMBR" ]; then
    osascript -e 'display alert "EMBR not found" message "Install EMBR.app or the embr command, then try again." as warning'
    exit 1
fi

[ $# -eq 0 ] && exit 0

dir=$(dirname "$1")

if [ $# -eq 1 ]; then
    base=$(basename "$1")
else
    base="Archive"
fi

out="$dir/$base.embr"
n=2
while [ -e "$out" ]; do
    out="$dir/$base $n.embr"
    n=$((n + 1))
done

if ! err=$("$EMBR" create "$out" "$@" -q 2>&1); then
    rm -f "$out"
    osascript -e "display alert \"EMBR could not create the archive\" message \"$err\" as warning"
    exit 1
fi
'''


def run_shell_script_action(script: str) -> dict:
    """One Automator 'Run Shell Script' action, configured to receive the
    selected paths as arguments rather than on stdin."""
    return {
        "action": {
            "AMAccepts": {
                "Container": "List",
                "Optional": True,
                "Types": ["com.apple.cocoa.string"],
            },
            "AMActionVersion": "2.0.3",
            "AMApplication": ["Automator"],
            "AMParameterProperties": {
                "COMMAND_STRING": {},
                "CheckedForUserDefaultShell": {},
                "inputMethod": {},
                "shell": {},
                "source": {},
            },
            "AMProvides": {
                "Container": "List",
                "Types": ["com.apple.cocoa.string"],
            },
            "ActionBundlePath": "/System/Library/Automator/Run Shell Script.action",
            "ActionName": "Run Shell Script",
            "ActionParameters": {
                "COMMAND_STRING": script,
                "CheckedForUserDefaultShell": True,
                # 1 = pass input "as arguments" ($@). 0 would pipe to stdin,
                # which loses the one-path-per-item boundary.
                "inputMethod": 1,
                "shell": "/bin/zsh",
                "source": "",
            },
            "BundleIdentifier": "com.apple.RunShellScript",
            "CFBundleVersion": "2.0.3",
            "CanShowSelectedItemsWhenRun": False,
            "CanShowWhenRun": True,
            "Category": ["AMCategoryUtilities"],
            "Class Name": "RunShellScriptAction",
            "InputUUID": str(uuid.uuid4()).upper(),
            "Keywords": ["Shell", "Script", "Command", "Run", "Unix"],
            "OutputUUID": str(uuid.uuid4()).upper(),
            "UUID": str(uuid.uuid4()).upper(),
            "UnlocalizedApplications": ["Automator"],
            "arguments": {},
            "isViewVisible": 1,
            "location": "309.000000:253.000000",
            "nibPath": "/System/Library/Automator/Run Shell Script.action/"
                       "Contents/Resources/Base.lproj/main.nib",
        },
        "isViewVisible": 1,
    }


def document_wflow() -> dict:
    return {
        "AMApplicationBuild": "521",
        "AMApplicationVersion": "2.10",
        "AMDocumentVersion": "2",
        "actions": [run_shell_script_action(SCRIPT)],
        "connectors": {},
        "workflowMetaData": {
            "applicationBundleIDsByPath": {},
            "applicationPaths": [],
            # Files and folders in, nothing out.
            "inputTypeIdentifier": "com.apple.Automator.fileSystemObject",
            "outputTypeIdentifier": "com.apple.Automator.nothing",
            "presentationMode": 11,
            "processesInput": 0,
            "serviceInputTypeIdentifier": "com.apple.Automator.fileSystemObject",
            "serviceProcessesInput": 0,
            "systemImageName": "NSTouchBarFolder",
            "useAutomaticInputType": 0,
            # This is what makes it a Service rather than a standalone workflow.
            "workflowTypeIdentifier": "com.apple.Automator.servicesMenu",
        },
    }


def info_plist() -> dict:
    return {
        "NSServices": [
            {
                "NSMenuItem": {"default": NAME},
                "NSMessage": "runWorkflowAsService",
                # Restrict to Finder; there is no sense offering this from a
                # text editor's Services menu.
                "NSRequiredContext": {"NSApplicationIdentifier": "com.apple.finder"},
                # public.item covers both files and folders.
                "NSSendFileTypes": ["public.item"],
            }
        ]
    }


def main() -> int:
    SERVICES.mkdir(parents=True, exist_ok=True)
    if WORKFLOW.exists():
        shutil.rmtree(WORKFLOW)
    contents = WORKFLOW / "Contents"
    contents.mkdir(parents=True)

    with (contents / "document.wflow").open("wb") as f:
        plistlib.dump(document_wflow(), f)
    with (contents / "Info.plist").open("wb") as f:
        plistlib.dump(info_plist(), f)

    # Tell the pasteboard server to re-read the services database, otherwise the
    # new item does not appear until the next login.
    pbs = "/System/Library/CoreServices/pbs"
    subprocess.run([pbs, "-flush"], capture_output=True)
    subprocess.run([pbs, "-update"], capture_output=True)

    # Verify it actually registered rather than trusting that writing the files
    # was enough.
    dump = subprocess.run([pbs, "-dump_pboard"], capture_output=True, text=True).stdout
    registered = NAME in dump

    print(f"installed {WORKFLOW}")
    if registered:
        print("    registered with the services database")
    else:
        print("    warning: not visible in the services database yet;")
        print("    it usually appears after logging out and back in")

    print()
    print(f'  Right-click a file or folder in Finder -> Quick Actions -> "{NAME}"')
    print("  Several items selected at once become one Archive.embr.")
    print()
    print("  If it is missing from the menu, enable it in")
    print("  System Settings -> General -> Login Items & Extensions -> Finder.")
    return 0 if registered else 0


if __name__ == "__main__":
    sys.exit(main())
