"""Makes a test-only binary variant with codex-whisply test support available."""

_DEFINE = "//command_line_option:define"
_TEST_SUPPORT_DEFINE = "codex_whisply_test_support"

def _whisply_test_support_transition_impl(settings, attr):
    defines = dict(settings[_DEFINE])
    defines[_TEST_SUPPORT_DEFINE] = "true"
    return {_DEFINE: defines}

_whisply_test_support_transition = transition(
    implementation = _whisply_test_support_transition_impl,
    inputs = [_DEFINE],
    outputs = [_DEFINE],
)

def _whisply_test_support_binary_impl(ctx):
    binary = ctx.attr.binary[DefaultInfo]
    runfiles = ctx.runfiles(transitive_files = binary.files)
    runfiles = runfiles.merge(binary.default_runfiles)
    return [
        DefaultInfo(
            # This wrapper exists only as integration-test data/runfile-env;
            # the transitioned executable remains owned by its original
            # target. Do not claim that artifact as this rule's executable.
            files = depset([ctx.executable.binary]),
            runfiles = runfiles,
        ),
    ]

whisply_test_support_binary = rule(
    implementation = _whisply_test_support_binary_impl,
    attrs = {
        "binary": attr.label(
            cfg = _whisply_test_support_transition,
            executable = True,
            mandatory = True,
        ),
        "_allowlist_function_transition": attr.label(
            default = "@bazel_tools//tools/allowlists/function_transition_allowlist",
        ),
    },
    doc = "Exposes a test-only app binary built with macOS codex-whisply test support.",
)
