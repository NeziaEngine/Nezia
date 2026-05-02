fn main() {
    println!("cargo:rerun-if-changed=src");

    let bindings_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("bindings");
    std::fs::create_dir_all(&bindings_dir).expect("create bindings dir");

    let inputs = [
        "src/types.rs",
        "src/engine.rs",
        "src/buffer.rs",
        "src/source.rs",
        "src/bus.rs",
        "src/spatial.rs",
    ];

    let mut builder = csbindgen::Builder::default()
        .csharp_class_name("LibNezia")
        .csharp_namespace("Nezia.Native")
        .csharp_dll_name("nezia")
        .csharp_entry_point_prefix("")
        .csharp_method_prefix("")
        .csharp_use_function_pointer(true);

    for input in inputs {
        builder = builder.input_extern_file(input);
    }

    builder
        .generate_csharp_file(bindings_dir.join("NeziaNative.g.cs"))
        .expect("generate C# bindings");
}
