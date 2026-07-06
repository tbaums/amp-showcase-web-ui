mod app;
mod models;
mod storage;
mod sync;

use app::App;

fn main() {
    leptos::mount::mount_to_body(App);
}
