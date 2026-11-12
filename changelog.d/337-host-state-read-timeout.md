---
kind: fixed
---
A `moc.host-state` plugin that hangs no longer makes its review host count as an outage: the required key `pohunek.host_state.read_timeout_seconds` (1 to 60, and at most `pohunek.timeout_seconds` minus 2, because the pohunek CLI waits 1 s longer for the answer) sets the daemon's time limit of the state read, and a read that the daemon stops, like any other `command_*` answer, is an unknown state that `unknown_state` decides.
