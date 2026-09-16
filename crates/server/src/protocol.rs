//! Server-only MCP protocol adapter.
//!
//! `ProtocolVersion` and [`NegotiatedFeatures`] must not leak into
//! `crates/domain`, `policy`, `patch`, or `runner`.

use rmcp::model::ProtocolVersion;

/// CI and product baseline. HTTP's spec floor is 2025-03-26; this is the pin.
pub const CORE_BASELINE: ProtocolVersion = ProtocolVersion::V_2025_11_25;

/// First MCP revision with Streamable HTTP. Not the CI baseline.
pub const HTTP_FLOOR: ProtocolVersion = ProtocolVersion::V_2025_03_26;

/// Optional progressive enhancement. Same `tools/call` semantics as the baseline.
pub const ENHANCEMENT: ProtocolVersion = ProtocolVersion::V_2026_07_28;

/// Versions this server will negotiate. 2024-11-05 HTTP+SSE is out of scope.
pub const SUPPORTED_PROTOCOL_VERSIONS: &[ProtocolVersion] = &[
    ProtocolVersion::V_2025_03_26,
    ProtocolVersion::V_2025_06_18,
    ProtocolVersion::V_2025_11_25,
    ProtocolVersion::V_2026_07_28,
];

/// Features implied by a negotiated MCP protocol revision.
///
/// `from_protocol` may set 2026-07-28 flags true. Handlers in this work
/// package must not take those paths: keep ordinary `tools/call`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NegotiatedFeatures {
    pub protocol: ProtocolVersion,
    pub mrtr: bool,
    pub tasks: bool,
    pub subscriptions: bool,
    pub standard_http_headers: bool,
    pub stateless_http: bool,
}

impl NegotiatedFeatures {
    /// Map a negotiated version to enhancement flags.
    ///
    /// 2025-11-25 (and older) keep every enhancement false. 2026-07-28 may
    /// set flags true for later progressive enhancement.
    pub fn from_protocol(protocol: ProtocolVersion) -> Self {
        let enhanced = protocol.as_str() >= ProtocolVersion::V_2026_07_28.as_str();
        Self {
            protocol,
            mrtr: enhanced,
            tasks: enhanced,
            subscriptions: enhanced,
            standard_http_headers: enhanced,
            stateless_http: enhanced,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baseline_disables_every_enhancement() {
        let features = NegotiatedFeatures::from_protocol(ProtocolVersion::V_2025_11_25);
        assert_eq!(features.protocol, ProtocolVersion::V_2025_11_25);
        assert!(!features.mrtr);
        assert!(!features.tasks);
        assert!(!features.subscriptions);
        assert!(!features.standard_http_headers);
        assert!(!features.stateless_http);
    }

    #[test]
    fn http_floor_is_not_0728_enhancement() {
        let features = NegotiatedFeatures::from_protocol(ProtocolVersion::V_2025_03_26);
        assert!(!features.mrtr);
        assert!(!features.standard_http_headers);
        assert!(!features.stateless_http);
    }

    #[test]
    fn enhancement_revision_may_set_flags_true() {
        let features = NegotiatedFeatures::from_protocol(ProtocolVersion::V_2026_07_28);
        assert_eq!(features.protocol, ProtocolVersion::V_2026_07_28);
        assert!(features.mrtr);
        assert!(features.tasks);
        assert!(features.subscriptions);
        assert!(features.standard_http_headers);
        assert!(features.stateless_http);
    }
}
