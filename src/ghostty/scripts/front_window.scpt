-- Id of the front Ghostty window, or empty when no window exists.
tell application "Ghostty"
	try
		return id of front window
	on error
		return ""
	end try
end tell
