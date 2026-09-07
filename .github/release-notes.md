### Install

```console
$ tar -xzf dirtbag-{{VERSION}}-aarch64-apple-darwin.tar.gz
$ sudo mv dirtbag-{{VERSION}}-aarch64-apple-darwin/dirtbag /usr/local/bin/
$ xattr -d com.apple.quarantine /usr/local/bin/dirtbag   # the binary is unsigned
```

Verify the download first:

```console
$ shasum -a 256 -c dirtbag-{{VERSION}}-aarch64-apple-darwin.tar.gz.sha256
```

Requires an Apple-Silicon Mac and [Tart](https://tart.run)
(`brew install cirruslabs/cli/tart`).
