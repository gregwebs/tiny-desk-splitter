# Just should call scripts

`just` is a CLI UI. It is not a place to write code.
`just` should invoke one line of shell script rather than writing multi-line shell code inside of just.

Advantages
* `just` is not required for the CI or for contributors of the release
* `just` is not required for contributors
* shellcheck will check all shell scripts
* not writing code inside code