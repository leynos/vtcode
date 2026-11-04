# diapason-acp

`diapason-acp` is the canonical ACP crate for Diapason.

It contains:

- The ACP client library now lives in this crate
- The Diapason Zed bridge and ACP connection registration helpers

<!-- cargo-rdme start -->

ACP (Agent Communication Protocol) support for Diapason.

This crate exposes both the ACP client library and the Diapason Zed bridge.
Downstream crates should treat this as the canonical ACP entrypoint.

<!-- cargo-rdme end -->

## Public entrypoints

- `AcpClientV2` for protocol-compliant ACP clients
- `AcpClient` for the deprecated V1 client API
- `StandardAcpAdapter` and `ZedAcpAdapter` for launching Diapason over ACP

## API reference

See [docs.rs/diapason-acp](https://docs.rs/diapason-acp).

## Related docs

- [ACP integration guide](../docs/acp/ACP_INTEGRATION.md)
- [ACP quick reference](../docs/acp/ACP_QUICK_REFERENCE.md)
