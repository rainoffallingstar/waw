Fixes #7.

`scoop install gh@2.7.0` regenerates the manifest from the bucket's current
definition and only keeps the version string, so the result can differ from
what 2.7.0 originally shipped. waw passed the version through faithfully but
said nothing, and `install --exact pkg@ver` reads as a reproducible pin.

Taking option 2 from the issue: warn on stderr, once per backend, naming the
versioned specs. Documented in `--help` and the README as the baseline in
option 1. The warning is deliberately not suppressible yet -- there is no
quiet flag, and inventing one here would be a second feature.

The version is still passed through: it is the only hint Scoop gets, so
swallowing it would make things worse rather than better. Refusing was ruled
out in the issue as too hostile.

Warns on both paths that can carry a version: `install --exact pkg@ver` and
`upgrade pkg@ver`. Interactive install cannot produce one (the user picks an
id from the search list), so it does not check.

Three unit tests: the capability flag per backend, and that Scoop still emits
the versioned argument with and without a version.
