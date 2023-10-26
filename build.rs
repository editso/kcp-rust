fn main() {
    cc::Build::new()
        .file("src/kcp/kcp_ext.c")
        .file("third_party/kcp/ikcp.c")
        .warnings(false)
        .include("third_party/kcp")
        .compile("kcp");
}
