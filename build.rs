fn main() {
    slint_build::compile_with_config(
        "ui/launcher.slint",
        slint_build::CompilerConfiguration::new().with_style("fluent".into()),
    )
    .expect("failed to compile Slint UI");
    embed_resource::compile("res/app.rc", embed_resource::NONE)
        .manifest_required()
        .expect("failed to embed resources");
    println!("cargo:rerun-if-changed=res");
}
