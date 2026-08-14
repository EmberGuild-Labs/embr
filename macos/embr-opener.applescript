-- EMBR.app — the Finder half of EMBR.
--
-- Two jobs:
--   1. Exist, so macOS has something to attach the .embr file type and its
--      icon to. Finder will not show a custom icon for a bare extension; the
--      icon has to be exported by an installed bundle that claims the type.
--   2. Extract a .embr when you double-click one, into a folder beside it,
--      the way Archive Utility handles a .zip.
--
-- Written as an AppleScript droplet because that is the smallest thing macOS
-- accepts as a document-handling application. The real work is done by the
-- `embr` binary copied into this bundle's Resources, so the app and the CLI
-- can never disagree about the format.
--
-- Deliberately does no cross-application scripting. An earlier version ended
-- with `tell application "Finder" to reveal` so the extracted folder popped
-- open; that needs Automation permission, and without it the droplet hangs
-- until the AppleEvent times out instead of extracting. Same reasoning rules
-- out notifications. Anything that needs the user to grant a permission before
-- a double-click works is not worth the convenience.

on open theFiles
	repeat with f in theFiles
		my extractOne(POSIX path of (f as alias))
	end repeat
end open

on extractOne(archivePath)
	set embrPath to POSIX path of (path to resource "embr")
	try
		set parentDir to do shell script "dirname " & quoted form of archivePath
		set baseName to do shell script "basename " & quoted form of archivePath & " .embr"

		-- Never overwrite: if "name" is taken, use "name 2", "name 3", ...
		set destPath to parentDir & "/" & baseName
		set n to 2
		repeat while my pathExists(destPath)
			set destPath to parentDir & "/" & baseName & " " & n
			set n to n + 1
		end repeat

		do shell script "mkdir -p " & quoted form of destPath & " && " & ¬
			quoted form of embrPath & " extract " & quoted form of archivePath & ¬
			" -C " & quoted form of destPath & " -q"
	on error errMsg
		-- display alert runs in this process, so it needs no permission and
		-- cannot hang the way a cross-app tell can.
		display alert "EMBR could not extract this archive" message errMsg as warning
	end try
end extractOne

on pathExists(p)
	try
		do shell script "test -e " & quoted form of p
		return true
	on error
		return false
	end try
end pathExists

-- Launched on its own rather than by opening a document.
--
-- Deliberately a no-op. build-app.sh has to launch this app once during
-- install, because macOS marks an app's exported type declarations "untrusted"
-- until it has run at least once and ignores an untrusted declaration's icon.
-- Anything shown here — dialog or notification — would either block that step
-- or need a permission grant. There is nothing to show anyway: this app is a
-- file handler, not something you launch on purpose.
on run
	return
end run
