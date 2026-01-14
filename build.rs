use cfg_aliases::cfg_aliases;

fn main() {
    cfg_aliases! {
        multithread: { any(feature = "tokio") },
    }
}
