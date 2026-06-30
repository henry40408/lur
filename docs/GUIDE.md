# lur guide

`lur` runs Luau in a sandbox. Two modes share one core: one-shot
`lur script.lua [args]` runs a script to completion; `lur serve app.lua` serves
it as a long-running HTTP server. Capabilities live under the `lur.*` global;
each is gated by a policy (default profile is `strict` — deny-all). See the
[README](../README.md) for the full flag set and the sandbox model.

Every example below is run as part of the test suite, so it stays correct.

## Data & I/O

### lur.json
### lur.base64
### lur.crypto
### lur.cookie
### lur.time
### lur.log
### lur.io

## State & arguments

### lur.args
### lur.state

## Capabilities (policy-gated)

### lur.fs
### lur.env
### lur.http

## Storage

### lur.db
### lur.kv

## Concurrency

### lur.async

## Server mode

### lur.serve
