#[expect(dead_code, reason = "called by the validate command, C01")]
mod config_file;
#[expect(dead_code, reason = "called by main, round 3 of C01")]
mod error_response;
#[expect(dead_code, reason = "called by main, round 3 of C01")]
mod logging;

fn main() {}
