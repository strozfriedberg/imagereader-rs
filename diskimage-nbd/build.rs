fn main() {
    // Embed the current commit hash (shared logic with the other binaries).
    buildinfo::emit_git_commit();
}
