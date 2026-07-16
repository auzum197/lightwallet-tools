# .onion endpoints: a sketch, not a plan

Deliberately high level. This exists because the CLI transport work ruled
`.onion` out of scope and the reasoning deserved a home.

## What it would mean

A lightwalletd reachable as a Tor hidden service, dialed by
`lightwallet-transport-tor` instead of exiting the Tor network to a clearnet
host. The metadata win over plain `--transport tor` is real but narrower than
it looks: exit-to-clearnet already hides the client from the server and the
server from the client's network. A hidden service additionally hides the
server's location and removes the exit hop as an observer.

## Why it is parked

- No target exists. Nobody in this repo's orbit runs a hidden-service
  lightwalletd, and none of the plans call for one.
- It is not free. Arti dials `.onion` only with the non-default
  `onion-service-client` feature plus a config allowance, and TLS-over-onion
  raises certificate questions (a `.onion` name won't appear in webpki-rooted
  certs, so the scheme-to-TLS rule needs a story).
- Untested code paths rot. Shipping the feature without a live endpoint to
  smoke against would violate the same principle the promotion gate encodes.

## What would unpark it

A real hidden-service lightwalletd worth pointing at. At that point the work
is contained: enable the arti feature, decide the TLS posture for `.onion`
names, and extend the transport crate's live suite to cover it. The CLI
inherits all of it for free, since the connector still yields a plain
`Channel`.
