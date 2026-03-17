# keramics

Keramics is a project that focusses on the analysis of data formats.

Project information:

- Status: experimental, pre-release

## FORK NOTES

This repository is forked from https://github.com/keramics/keramics.

I forked it to extend concurrent-read features for keramics-formats, however it is not easy to modify the current structure, so I created a new crate named `keramics-drivers` to store the new concurrent code.

Since this repository is detached from upstream, you can only use it as a git reference:

```toml
keramics-drivers = { git = "https://github.com/Reverier-Xu/keramics" }
```

Thanks for the awesome original works!
