use sea_orm_migration::prelude::*;

pub struct Migrator;

impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(CreateProxyEvents)]
    }
}

#[derive(DeriveMigrationName)]
struct CreateProxyEvents;

#[async_trait::async_trait]
impl MigrationTrait for CreateProxyEvents {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(ProxyEvents::Table)
                    .if_not_exists()
                    .col(string(ProxyEvents::EventId).primary_key())
                    .col(string(ProxyEvents::ConnectionId))
                    .col(string(ProxyEvents::RequestStartTs))
                    .col(string(ProxyEvents::RequestEndTs))
                    .col(big_integer(ProxyEvents::DurationMs))
                    .col(string_null(ProxyEvents::ClientIp))
                    .col(integer_null(ProxyEvents::ClientPort))
                    .col(string_null(ProxyEvents::ProxyLocalIp))
                    .col(integer_null(ProxyEvents::ProxyLocalPort))
                    .col(string_null(ProxyEvents::FrontendServerName))
                    .col(string_null(ProxyEvents::FrontendHttpVersion))
                    .col(string(ProxyEvents::FrontendScheme))
                    .col(string(ProxyEvents::BackendUrl))
                    .col(string(ProxyEvents::BackendHost))
                    .col(string_null(ProxyEvents::BackendIp))
                    .col(integer_null(ProxyEvents::BackendPort))
                    .col(string_null(ProxyEvents::BackendHttpVersion))
                    .col(boolean_null(ProxyEvents::UpstreamConnectionReused))
                    .col(string(ProxyEvents::Method))
                    .col(string_null(ProxyEvents::Authority))
                    .col(string(ProxyEvents::Path))
                    .col(string_null(ProxyEvents::Query))
                    .col(text(ProxyEvents::RequestHeadersJson))
                    .col(text_null(ProxyEvents::RequestCookieHeader))
                    .col(text_null(ProxyEvents::RequestCookiesJson))
                    .col(big_integer_null(ProxyEvents::RequestContentLength))
                    .col(string_null(ProxyEvents::RequestTransferEncoding))
                    .col(string_null(ProxyEvents::RequestContentType))
                    .col(string_null(ProxyEvents::RequestBodySha256))
                    .col(text_null(ProxyEvents::RequestBodyPreviewBase64))
                    .col(boolean_null(ProxyEvents::RequestBodyTruncated))
                    .col(integer_null(ProxyEvents::StatusCode))
                    .col(string_null(ProxyEvents::ReasonPhrase))
                    .col(text_null(ProxyEvents::ResponseHeadersJson))
                    .col(text_null(ProxyEvents::ResponseSetCookieJson))
                    .col(big_integer_null(ProxyEvents::ResponseContentLength))
                    .col(string_null(ProxyEvents::ResponseTransferEncoding))
                    .col(string_null(ProxyEvents::ResponseContentType))
                    .col(string_null(ProxyEvents::ResponseContentEncoding))
                    .col(string_null(ProxyEvents::ResponseBodySha256))
                    .col(text_null(ProxyEvents::ResponseBodyPreviewBase64))
                    .col(boolean_null(ProxyEvents::ResponseBodyTruncated))
                    .col(string_null(ProxyEvents::FrontendTlsVersion))
                    .col(string_null(ProxyEvents::FrontendTlsCipherSuite))
                    .col(string_null(ProxyEvents::FrontendTlsAlpn))
                    .col(string_null(ProxyEvents::FrontendTlsSni))
                    .col(string_null(ProxyEvents::FrontendTlsJa3))
                    .col(string_null(ProxyEvents::FrontendTlsJa4))
                    .col(string_null(ProxyEvents::BackendTlsVersion))
                    .col(string_null(ProxyEvents::BackendTlsCipherSuite))
                    .col(string_null(ProxyEvents::BackendTlsAlpn))
                    .col(string_null(ProxyEvents::BackendTlsSni))
                    .col(string_null(ProxyEvents::BackendCertLeafSha256))
                    .col(text_null(ProxyEvents::BackendCertSubject))
                    .col(text_null(ProxyEvents::BackendCertIssuer))
                    .col(string_null(ProxyEvents::BackendCertNotBefore))
                    .col(string_null(ProxyEvents::BackendCertNotAfter))
                    .col(big_integer_null(ProxyEvents::DnsDurationMs))
                    .col(big_integer_null(ProxyEvents::ConnectDurationMs))
                    .col(big_integer_null(ProxyEvents::TlsHandshakeDurationMs))
                    .col(big_integer_null(ProxyEvents::UpstreamFirstByteMs))
                    .col(big_integer_null(ProxyEvents::ResponseStreamDurationMs))
                    .col(string_null(ProxyEvents::ErrorKind))
                    .col(text_null(ProxyEvents::ErrorText))
                    .col(string(ProxyEvents::ProxyResult))
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum ProxyEvents {
    Table,
    EventId,
    ConnectionId,
    RequestStartTs,
    RequestEndTs,
    DurationMs,
    ClientIp,
    ClientPort,
    ProxyLocalIp,
    ProxyLocalPort,
    FrontendServerName,
    FrontendHttpVersion,
    FrontendScheme,
    BackendUrl,
    BackendHost,
    BackendIp,
    BackendPort,
    BackendHttpVersion,
    UpstreamConnectionReused,
    Method,
    Authority,
    Path,
    Query,
    RequestHeadersJson,
    RequestCookieHeader,
    RequestCookiesJson,
    RequestContentLength,
    RequestTransferEncoding,
    RequestContentType,
    RequestBodySha256,
    RequestBodyPreviewBase64,
    RequestBodyTruncated,
    StatusCode,
    ReasonPhrase,
    ResponseHeadersJson,
    ResponseSetCookieJson,
    ResponseContentLength,
    ResponseTransferEncoding,
    ResponseContentType,
    ResponseContentEncoding,
    ResponseBodySha256,
    ResponseBodyPreviewBase64,
    ResponseBodyTruncated,
    FrontendTlsVersion,
    FrontendTlsCipherSuite,
    FrontendTlsAlpn,
    FrontendTlsSni,
    FrontendTlsJa3,
    FrontendTlsJa4,
    BackendTlsVersion,
    BackendTlsCipherSuite,
    BackendTlsAlpn,
    BackendTlsSni,
    BackendCertLeafSha256,
    BackendCertSubject,
    BackendCertIssuer,
    BackendCertNotBefore,
    BackendCertNotAfter,
    DnsDurationMs,
    ConnectDurationMs,
    TlsHandshakeDurationMs,
    UpstreamFirstByteMs,
    ResponseStreamDurationMs,
    ErrorKind,
    ErrorText,
    ProxyResult,
}

fn string(column: ProxyEvents) -> ColumnDef {
    ColumnDef::new(column).string().not_null().to_owned()
}

fn string_null(column: ProxyEvents) -> ColumnDef {
    ColumnDef::new(column).string().null().to_owned()
}

fn text(column: ProxyEvents) -> ColumnDef {
    ColumnDef::new(column).text().not_null().to_owned()
}

fn text_null(column: ProxyEvents) -> ColumnDef {
    ColumnDef::new(column).text().null().to_owned()
}

fn integer_null(column: ProxyEvents) -> ColumnDef {
    ColumnDef::new(column).integer().null().to_owned()
}

fn big_integer(column: ProxyEvents) -> ColumnDef {
    ColumnDef::new(column).big_integer().not_null().to_owned()
}

fn big_integer_null(column: ProxyEvents) -> ColumnDef {
    ColumnDef::new(column).big_integer().null().to_owned()
}

fn boolean_null(column: ProxyEvents) -> ColumnDef {
    ColumnDef::new(column).boolean().null().to_owned()
}
