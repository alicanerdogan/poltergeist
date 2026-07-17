-- One record per terminal, fields denormalized:
-- window-id US tab-id US tab-index US tab-selected US terminal-id US cwd US tab-name US terminal-name US window-front RS
set US to ASCII character 31
set RS to ASCII character 30
set rows to {}
tell application "Ghostty"
	try
		set frontId to id of front window
	on error
		set frontId to ""
	end try
	repeat with w in windows
		set wid to id of w
		set isFront to (wid is frontId) as text
		repeat with t in tabs of w
			set tid to id of t
			set tidx to index of t
			set tsel to selected of t
			try
				set tname to name of t
			on error
				set tname to ""
			end try
			repeat with term in terminals of t
				set termid to id of term
				try
					set tcwd to working directory of term
				on error
					set tcwd to ""
				end try
				try
					set termname to name of term
				on error
					set termname to ""
				end try
				set end of rows to wid & US & tid & US & tidx & US & tsel & US & termid & US & tcwd & US & tname & US & termname & US & isFront
			end repeat
		end repeat
	end repeat
end tell
set AppleScript's text item delimiters to RS
return rows as text
