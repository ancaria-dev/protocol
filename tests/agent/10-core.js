// Only what the bundler asserts about: the declaration order of NO_HOOK
// against core's reader, and that core is never dropped.
function hook(name, rva, callbacks) {
    if (NO_HOOK.indexOf(name) >= 0) { return; }
}
