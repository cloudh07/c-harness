fn main() {
    println!("cargo:rerun-if-changed=resources/c-harness.rc");
    println!("cargo:rerun-if-changed=resources/c-harness.ico");
    // Does nothing when the target is not Windows.
    if let Err(err) =
        embed_resource::compile("resources/c-harness.rc", embed_resource::NONE).manifest_optional()
    {
        panic!("could not embed the Windows icon: {err}");
    }
}
