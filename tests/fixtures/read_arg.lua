local path = lur.args.positional[1]
lur.stdout.write(lur.fs.read(path))
