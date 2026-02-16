use wl_client_builder::Builder;

fn main() {
    Builder::default().with_mutable_data(true).build().unwrap();
}
