//! Production enrichment: [`live`] holds the search counts and the online
//! lyrics built at boot; [`library`] is Library Management's enrichment
//! port.

pub mod library;
pub mod live;
