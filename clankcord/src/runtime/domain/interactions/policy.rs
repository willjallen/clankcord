pub fn requires_confirmation(command_kind: &str) -> bool {
    command_kind == "forget_window"
}
