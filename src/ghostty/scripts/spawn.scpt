-- Creates surfaces. argv:
--   1: op            new_tab | new_window | split
--   2: target        window id (new_tab) | terminal id (split) | "" (new_window)
--   3: direction     right | down (split only)
--   4: cwd           initial working directory, "" = omit
--   5: env           KEY=VALUE entries joined by ASCII 31, "" = omit
-- Output: new_tab/new_window -> window-id US tab-id US terminal-id
--         split              -> terminal-id
on splitText(theText, theDelim)
	set oldDelims to AppleScript's text item delimiters
	set AppleScript's text item delimiters to theDelim
	set theItems to text items of theText
	set AppleScript's text item delimiters to oldDelims
	return theItems
end splitText

on makeConfig(cwd, envList)
	tell application "Ghostty"
		set rec to {}
		if cwd is not "" then set rec to rec & {initial working directory:cwd}
		if envList is not {} then set rec to rec & {environment variables:envList}
		if rec is {} then return missing value
		return new surface configuration from rec
	end tell
end makeConfig

on run argv
	set US to ASCII character 31
	set op to item 1 of argv
	set target to item 2 of argv
	set dir to item 3 of argv
	set cwd to item 4 of argv
	set envText to item 5 of argv
	set envList to {}
	if envText is not "" then set envList to my splitText(envText, US)
	set cfg to my makeConfig(cwd, envList)
	tell application "Ghostty"
		if op is "new_window" then
			if cfg is missing value then
				set w to new window
			else
				set w to new window with configuration cfg
			end if
			set t to selected tab of w
			set term to focused terminal of t
			return (id of w as text) & US & (id of t as text) & US & (id of term as text)
		else if op is "new_tab" then
			set w to first window whose id is target
			if cfg is missing value then
				set t to new tab in w
			else
				set t to new tab in w with configuration cfg
			end if
			set term to focused terminal of t
			return (id of w as text) & US & (id of t as text) & US & (id of term as text)
		else if op is "split" then
			set term to first terminal whose id is target
			if dir is "down" then
				set theDir to down
			else
				set theDir to right
			end if
			if cfg is missing value then
				set newTerm to split term direction theDir
			else
				set newTerm to split term direction theDir with configuration cfg
			end if
			return id of newTerm as text
		else
			error "unknown spawn op: " & op
		end if
	end tell
end run
