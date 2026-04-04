
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum Informational {
    Continue = 100,
    SwitchingProtocols = 101,
    Processing = 102,
    EarlyHints = 103,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum Success {
    Ok = 200,
    Created = 201,
    Accepted = 202,
    NonAuthoritativeInformation = 203,
    NoContent = 204,
    ResetContent = 205,
    PartialContent = 206,
    MultiStatus = 207,
    AlreadyReported = 208,
    ImUsed = 226,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum Redirection {
    MultipleChoices = 300,
    MovedPermanently = 301,
    Found = 302,
    SeeOther = 303,
    NotModified = 304,
    UseProxy = 305,
    TemporaryRedirect = 307,
    PermanentRedirect = 308,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum ClientError {
    BadRequest = 400,
    Unauthorised = 401,
    PaymentRequired = 402,
    Forbidden = 403,
    NotFound = 404,
    MethodNotAllowed = 405,
    NotAcceptable = 406,
    ProxyAuthenticationRequired = 407,
    RequestTimeout = 408,
    Conflict = 409,
    Gone = 410,
    LengthRequired = 411,
    PreconditionFailed = 412,
    PayloadTooLarge = 413,
    UriTooLong = 414,
    UnsupportedMediaType = 415,
    RangeNotSatisfiable = 416,
    ExceptionFailed = 417,
    ImATeapot = 418,
    MisdirectedRequest = 421,
    UnprocessableEntity = 422,
    Locked = 423,
    FailedDependency = 424,
    TooEarly = 425,
    UpgradeRequired = 426,
    PreconditionRequired = 428,
    TooManyRequests = 429,
    RequestHeaderFieldsTooLarge = 431,
    UnavailableForLegalReasons = 451,
    ClientClosedRequest = 499,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum ServerError {
    InternalServerError = 500,
    NotImplemented = 501,
    BadGateway = 502,
    ServiceUnavailable = 503,
    GatewayTimeout = 504,
    HttpVersionNotSupported = 505,
    VariantAlsoNegotiates = 506,
    InsufficientStorage = 507,
    LoopDetected = 508,
    NotExtended = 510,
    NetworkAuthenticationRequired = 511,
    NetworkConnectionTimeoutError = 599,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Client(ClientError),
    Server(ServerError),
}

impl Error {
    pub fn code(&self) -> u16 {
        match self {
            Self::Client(s) => *s as u16,
            Self::Server(s) => *s as u16,
        }
    }

    pub fn reason_phrase(&self) -> &'static str {
        match self {
            Self::Client(s) => Code::ClientError(*s).reason_phrase(),
            Self::Server(s) => Code::ServerError(*s).reason_phrase(),
        }
    }
}

pub enum Complete {
    Informational(Informational),
    Success(Success),
    Redirection(Redirection),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Code {
    Informational(Informational),
    Success(Success),
    Redirection(Redirection),
    ClientError(ClientError),
    ServerError(ServerError),
}

impl Code {
    pub fn code(&self) -> u16 {
        match self {
            Self::Informational(s) => *s as u16,
            Self::Success(s) => *s as u16,
            Self::Redirection(s) => *s as u16,
            Self::ClientError(s) => *s as u16,
            Self::ServerError(s) => *s as u16,
        }
    }

    pub fn as_error(&self) -> Option<Error> {
        match self {
            Self::ClientError(s) => Some(Error::Client(*s)),
            Self::ServerError(s) => Some(Error::Server(*s)),
            _ => None,
        }
    }

    pub fn is_error(&self) -> bool {
        matches!(self, Self::ClientError(_) | Self::ServerError(_))
    }

    pub fn is_success(&self) -> bool {
        matches!(self, Self::Success(_))
    }

    pub fn reason_phrase(&self) -> &'static str {
        match self {
            Self::Informational(s) => match s {
                Informational::Continue => "Continue",
                Informational::SwitchingProtocols => "Switching Protocols",
                Informational::Processing => "Processing",
                Informational::EarlyHints => "Early Hints",
            },
            Self::Success(s) => match s {
                Success::Ok => "OK",
                Success::Created => "Created",
                Success::Accepted => "Accepted",
                Success::NonAuthoritativeInformation => "Non-Authoritative Information",
                Success::NoContent => "No Content",
                Success::ResetContent => "Reset Content",
                Success::PartialContent => "Partial Content",
                Success::MultiStatus => "Multi-Status",
                Success::AlreadyReported => "Already Reported",
                Success::ImUsed => "IM Used",
            },
            Self::Redirection(s) => match s {
                Redirection::MultipleChoices => "Multiple Choices",
                Redirection::MovedPermanently => "Moved Permanently",
                Redirection::Found => "Found",
                Redirection::SeeOther => "See Other",
                Redirection::NotModified => "Not Modified",
                Redirection::UseProxy => "Use Proxy",
                Redirection::TemporaryRedirect => "Temporary Redirect",
                Redirection::PermanentRedirect => "Permanent Redirect",
            },
            Self::ClientError(s) => match s {
                ClientError::BadRequest => "Bad Request",
                ClientError::Unauthorised => "Unauthorized",
                ClientError::PaymentRequired => "Payment Required",
                ClientError::Forbidden => "Forbidden",
                ClientError::NotFound => "Not Found",
                ClientError::MethodNotAllowed => "Method Not Allowed",
                ClientError::NotAcceptable => "Not Acceptable",
                ClientError::ProxyAuthenticationRequired => "Proxy Authentication Required",
                ClientError::RequestTimeout => "Request Timeout",
                ClientError::Conflict => "Conflict",
                ClientError::Gone => "Gone",
                ClientError::LengthRequired => "Length Required",
                ClientError::PreconditionFailed => "Precondition Failed",
                ClientError::PayloadTooLarge => "Payload Too Large",
                ClientError::UriTooLong => "URI Too Long",
                ClientError::UnsupportedMediaType => "Unsupported Media Type",
                ClientError::RangeNotSatisfiable => "Range Not Satisfiable",
                ClientError::ExceptionFailed => "Expectation Failed",
                ClientError::ImATeapot => "I'm a teapot",
                ClientError::MisdirectedRequest => "Misdirected Request",
                ClientError::UnprocessableEntity => "Unprocessable Entity",
                ClientError::Locked => "Locked",
                ClientError::FailedDependency => "Failed Dependency",
                ClientError::TooEarly => "Too Early",
                ClientError::UpgradeRequired => "Upgrade Required",
                ClientError::PreconditionRequired => "Precondition Required",
                ClientError::TooManyRequests => "Too Many Requests",
                ClientError::RequestHeaderFieldsTooLarge => "Request Header Fields Too Large",
                ClientError::UnavailableForLegalReasons => "Unavailable For Legal Reasons",
                ClientError::ClientClosedRequest => "Client Closed Request",
            },
            Self::ServerError(s) => match s {
                ServerError::InternalServerError => "Internal Server Error",
                ServerError::NotImplemented => "Not Implemented",
                ServerError::BadGateway => "Bad Gateway",
                ServerError::ServiceUnavailable => "Service Unavailable",
                ServerError::GatewayTimeout => "Gateway Timeout",
                ServerError::HttpVersionNotSupported => "HTTP Version Not Supported",
                ServerError::VariantAlsoNegotiates => "Variant Also Negotiates",
                ServerError::InsufficientStorage => "Insufficient Storage",
                ServerError::LoopDetected => "Loop Detected",
                ServerError::NotExtended => "Not Extended",
                ServerError::NetworkAuthenticationRequired => "Network Authentication Required",
                ServerError::NetworkConnectionTimeoutError => "Network Connection Timeout Error",
            },
        }
    }
}

impl std::convert::TryFrom<u16> for Code {
    type Error = ();

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        match value {
            100 => Ok(Self::Informational(Informational::Continue)),
            101 => Ok(Self::Informational(Informational::SwitchingProtocols)),
            102 => Ok(Self::Informational(Informational::Processing)),
            103 => Ok(Self::Informational(Informational::EarlyHints)),
            
            200 => Ok(Self::Success(Success::Ok)),
            201 => Ok(Self::Success(Success::Created)),
            202 => Ok(Self::Success(Success::Accepted)),
            203 => Ok(Self::Success(Success::NonAuthoritativeInformation)),
            204 => Ok(Self::Success(Success::NoContent)),
            205 => Ok(Self::Success(Success::ResetContent)),
            206 => Ok(Self::Success(Success::PartialContent)),
            207 => Ok(Self::Success(Success::MultiStatus)),
            208 => Ok(Self::Success(Success::AlreadyReported)),
            226 => Ok(Self::Success(Success::ImUsed)),
            
            300 => Ok(Self::Redirection(Redirection::MultipleChoices)),
            301 => Ok(Self::Redirection(Redirection::MovedPermanently)),
            302 => Ok(Self::Redirection(Redirection::Found)),
            303 => Ok(Self::Redirection(Redirection::SeeOther)),
            304 => Ok(Self::Redirection(Redirection::NotModified)),
            305 => Ok(Self::Redirection(Redirection::UseProxy)),
            307 => Ok(Self::Redirection(Redirection::TemporaryRedirect)),
            308 => Ok(Self::Redirection(Redirection::PermanentRedirect)),
            
            400 => Ok(Self::ClientError(ClientError::BadRequest)),
            401 => Ok(Self::ClientError(ClientError::Unauthorised)),
            402 => Ok(Self::ClientError(ClientError::PaymentRequired)),
            403 => Ok(Self::ClientError(ClientError::Forbidden)),
            404 => Ok(Self::ClientError(ClientError::NotFound)),
            405 => Ok(Self::ClientError(ClientError::MethodNotAllowed)),
            406 => Ok(Self::ClientError(ClientError::NotAcceptable)),
            407 => Ok(Self::ClientError(ClientError::ProxyAuthenticationRequired)),
            408 => Ok(Self::ClientError(ClientError::RequestTimeout)),
            409 => Ok(Self::ClientError(ClientError::Conflict)),
            410 => Ok(Self::ClientError(ClientError::Gone)),
            411 => Ok(Self::ClientError(ClientError::LengthRequired)),
            412 => Ok(Self::ClientError(ClientError::PreconditionFailed)),
            413 => Ok(Self::ClientError(ClientError::PayloadTooLarge)),
            414 => Ok(Self::ClientError(ClientError::UriTooLong)),
            415 => Ok(Self::ClientError(ClientError::UnsupportedMediaType)),
            416 => Ok(Self::ClientError(ClientError::RangeNotSatisfiable)),
            417 => Ok(Self::ClientError(ClientError::ExceptionFailed)),
            418 => Ok(Self::ClientError(ClientError::ImATeapot)),
            421 => Ok(Self::ClientError(ClientError::MisdirectedRequest)),
            422 => Ok(Self::ClientError(ClientError::UnprocessableEntity)),
            423 => Ok(Self::ClientError(ClientError::Locked)),
            424 => Ok(Self::ClientError(ClientError::FailedDependency)),
            425 => Ok(Self::ClientError(ClientError::TooEarly)),
            426 => Ok(Self::ClientError(ClientError::UpgradeRequired)),
            428 => Ok(Self::ClientError(ClientError::PreconditionRequired)),
            429 => Ok(Self::ClientError(ClientError::TooManyRequests)),
            431 => Ok(Self::ClientError(ClientError::RequestHeaderFieldsTooLarge)),
            451 => Ok(Self::ClientError(ClientError::UnavailableForLegalReasons)),
            499 => Ok(Self::ClientError(ClientError::ClientClosedRequest)),
            
            500 => Ok(Self::ServerError(ServerError::InternalServerError)),
            501 => Ok(Self::ServerError(ServerError::NotImplemented)),
            502 => Ok(Self::ServerError(ServerError::BadGateway)),
            503 => Ok(Self::ServerError(ServerError::ServiceUnavailable)),
            504 => Ok(Self::ServerError(ServerError::GatewayTimeout)),
            505 => Ok(Self::ServerError(ServerError::HttpVersionNotSupported)),
            506 => Ok(Self::ServerError(ServerError::VariantAlsoNegotiates)),
            507 => Ok(Self::ServerError(ServerError::InsufficientStorage)),
            508 => Ok(Self::ServerError(ServerError::LoopDetected)),
            510 => Ok(Self::ServerError(ServerError::NotExtended)),
            511 => Ok(Self::ServerError(ServerError::NetworkAuthenticationRequired)),
            599 => Ok(Self::ServerError(ServerError::NetworkConnectionTimeoutError)),
            _ => Err(()),
        }
    }
}

#[derive(Debug)]
pub struct HttpError {
    pub status: Error,
    pub message: Option<String>,
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HTTP {}: {}", self.status.code(), self.status.reason_phrase())?;
        if let Some(msg) = &self.message {
            write!(f, " ({})", msg)?;
        }
        Ok(())
    }
}

impl std::error::Error for HttpError {}
