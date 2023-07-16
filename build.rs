fn main() {
    cc::Build::new()
        .file("third_party/kcp/ikcp.c")
        .include("third_party/kcp")
        .compile("kcp");
}
