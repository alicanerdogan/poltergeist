-- Acts on existing surfaces, addressed by stable ids rebuilt via `whose`.
-- argv:
--   1: op            activate_app | perform_action | input_text | send_enter
--                    | activate_window | select_tab | close_tab | focus
--   2: window/terminal id (op-dependent)
--   3: tab id / action string / text (op-dependent)
on run argv
	set op to item 1 of argv
	tell application "Ghostty"
		if op is "activate_app" then
			activate
		else if op is "perform_action" then
			set term to first terminal whose id is (item 2 of argv)
			perform action (item 3 of argv) on term
		else if op is "input_text" then
			set term to first terminal whose id is (item 2 of argv)
			input text (item 3 of argv) to term
		else if op is "send_enter" then
			set term to first terminal whose id is (item 2 of argv)
			send key "enter" to term
		else if op is "activate_window" then
			activate window (first window whose id is (item 2 of argv))
		else if op is "select_tab" then
			set w to first window whose id is (item 2 of argv)
			select tab (first tab of w whose id is (item 3 of argv))
		else if op is "close_tab" then
			set w to first window whose id is (item 2 of argv)
			close tab (first tab of w whose id is (item 3 of argv))
		else if op is "focus" then
			focus (first terminal whose id is (item 2 of argv))
		else
			error "unknown action op: " & op
		end if
	end tell
end run
