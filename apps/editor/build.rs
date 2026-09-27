fn main() {
    // Debug info gives elements their ids/type names in Slint's MCP
    // element tree, which is how Claude finds and clicks them.
    let config = slint_build::CompilerConfiguration::new().with_style("fluent-dark".into()).with_debug_info(true);
    slint_build::compile_with_config("ui/app.slint", config).expect("slint compile");
}
