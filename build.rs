use cfg_aliases::cfg_aliases;

fn main() {
    cfg_aliases! {
        send_rt: { any(feature = "tokio-rt") },
    }
}
