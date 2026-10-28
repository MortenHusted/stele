mod service;

fn main() {
    dispatch(service::handle);
}

fn dispatch(callback: fn()) {
    callback();
}
