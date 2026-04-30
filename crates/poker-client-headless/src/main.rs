//! Ad-hoc reproduction binary. Exists so a developer can manually
//! drive a `HeadlessClient` from a terminal when chasing a bug;
//! production / CI doesn't run this.

use poker_client_core::{Intent, Phase};
use poker_client_headless::HeadlessClient;

fn main() {
    let mut h = HeadlessClient::new(None);
    h.intent(Intent::Connect {
        addr: "127.0.0.1:7878".into(),
    });
    h.assert_phase(&Phase::Connecting);
    println!("headless: phase = {:?}", h.view().phase);
    println!(
        "headless: emitted {} effect(s) so far",
        h.effect_log().len()
    );
    println!("headless: skeleton — implementation in step 25c follow-up");
}
