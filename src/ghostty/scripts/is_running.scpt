-- Liveness check that never launches Ghostty (spec §5.3).
tell application "System Events" to (name of processes) contains "Ghostty"
