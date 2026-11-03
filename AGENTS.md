# Lifecycle design goals

Apply these goals when changing connection, audio, media-control or resource-lifetime code:

- **One explicit lifecycle:** clear sequential states from discovering the pods through hearing audio.
- **Visible parallel work:** obvious launch points for connection, profile and codec tasks, and clear rules for when their completion blocks progress.
- **Single ownership:** one place responsible for starting, cancelling, retrying and releasing each resource.
- **Shared retry mechanics:** consistent timeouts, backoff and attempt tracking, while keeping recovery decisions with the component that understands them.
- **Less production logic:** remove duplicate state, overlapping workers and redundant transition paths.
- **Preserved behavior:** fast connections and takeovers, correct media controls, and no unnecessary interruptions.

The measure of success is fewer places to understand or change a connection's lifetime, not more labels describing the existing flow. New abstractions should earn their place by replacing duplicated decisions or bookkeeping, rather than wrapping them.

See [the connection lifecycle documentation](docs/connection-lifecycle.md) for the current ownership, launch points and transition rules.
