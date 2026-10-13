//! Prints the pill over a scripted agent turn (with an approval prompt):
//! `cargo run -p rozu-activity --example replay`.

use rozu_activity::replay::{demo_turn, replay};

fn main() {
    for (t, activity) in replay(&demo_turn()) {
        match activity {
            None => println!("{t:6.2}s  (idle)"),
            Some(a) => {
                let mut line = format!("{t:6.2}s  {:<9} {}", format!("{:?}", a.state), a.label);
                if let Some(detail) = &a.detail {
                    line.push_str(&format!(" · {detail}"));
                }
                if let Some(hint) = a.hint {
                    line.push_str(&format!(" · {hint}"));
                }
                println!("{line}");
            }
        }
    }
}
