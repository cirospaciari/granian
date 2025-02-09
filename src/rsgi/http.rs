use hyper::{
    Body,
    Request,
    Response,
    StatusCode,
    header::SERVER as HK_SERVER,
    http::response::Builder as ResponseBuilder
};
use std::net::SocketAddr;
use tokio::{fs::File, sync::mpsc};
use tokio_util::codec::{BytesCodec, FramedRead};

use crate::{
    callbacks::CallbackWrapper,
    http::{HV_SERVER, response_500},
    runtime::RuntimeRef,
    ws::{UpgradeData, is_upgrade_request as is_ws_upgrade, upgrade_intent as ws_upgrade}
};
use super::{
    callbacks::{call_rtb_http, call_rtb_ws, call_rtt_http, call_rtt_ws},
    types::{ResponseType, RSGIScope as Scope}
};


async fn file_body(file_path: String) -> Body {
    let file = File::open(file_path).await.unwrap();
    let stream = FramedRead::new(file, BytesCodec::new());
    Body::wrap_stream(stream)
}

macro_rules! default_scope {
    ($server_addr:expr, $client_addr:expr, $req:expr, $scheme:expr) => {
        Scope::new(
            "http",
            $req.version(),
            $scheme,
            $req.uri().clone(),
            $req.method().as_ref(),
            $server_addr,
            $client_addr,
            $req.headers()
        )
    };
}

macro_rules! handle_http_response {
    ($handler:expr, $rt:expr, $callback:expr, $req:expr, $scope:expr) => {
        match $handler($callback, $rt, $req, $scope).await {
            Ok(pyres) => {
                let res = match pyres.mode {
                    ResponseType::Body => {
                        pyres.inner.body(pyres.body)
                    },
                    ResponseType::File => {
                        pyres.inner.body(file_body(pyres.file.unwrap()).await)
                    }
                };
                match res {
                    Ok(res) => res,
                    _ => response_500()
                }
            },
            _ => response_500()
        }
    };
}

macro_rules! handle_request {
    ($func_name:ident, $handler:expr) => {
        pub(crate) async fn $func_name(
            rt: RuntimeRef,
            callback: CallbackWrapper,
            server_addr: SocketAddr,
            client_addr: SocketAddr,
            req: Request<Body>,
            scheme: &str
        ) -> Response<Body> {
            let scope = default_scope!(server_addr, client_addr, &req, scheme);
            handle_http_response!($handler, rt, callback, req, scope)
        }
    };
}

macro_rules! handle_request_with_ws {
    ($func_name:ident, $handler_req:expr, $handler_ws:expr) => {
        pub(crate) async fn $func_name(
            rt: RuntimeRef,
            callback: CallbackWrapper,
            server_addr: SocketAddr,
            client_addr: SocketAddr,
            req: Request<Body>,
            scheme: &str
        ) -> Response<Body> {
            let mut scope = default_scope!(server_addr, client_addr, &req, scheme);

            if is_ws_upgrade(&req) {
                scope.set_proto("ws");

                match ws_upgrade(req, None) {
                    Ok((res, ws)) => {
                        let rth = rt.clone();
                        let (restx, mut resrx) = mpsc::channel(1);

                        rt.inner.spawn(async move {
                            let tx_ref = restx.clone();

                            match $handler_ws(
                                callback,
                                rth,
                                ws,
                                UpgradeData::new(res, restx),
                                scope
                            ).await {
                                Ok((status, consumed)) => {
                                    if !consumed {
                                        let _ = tx_ref.send(
                                            ResponseBuilder::new()
                                                .status(
                                                    StatusCode::from_u16(status as u16)
                                                        .unwrap_or(StatusCode::FORBIDDEN)
                                                )
                                                .header(HK_SERVER, HV_SERVER)
                                                .body(Body::from(""))
                                                .unwrap()
                                        ).await;
                                    }
                                },
                                _ => {
                                    let _ = tx_ref.send(response_500()).await;
                                }
                            }
                        });

                        return match resrx.recv().await {
                            Some(res) => {
                                resrx.close();
                                res
                            },
                            _ => response_500()
                        }
                    },
                    Err(err) => {
                        return ResponseBuilder::new()
                            .status(StatusCode::BAD_REQUEST)
                            .header(HK_SERVER, HV_SERVER)
                            .body(Body::from(format!("{}", err)))
                            .unwrap()
                    }
                }
            }

            handle_http_response!($handler_req, rt, callback, req, scope)
        }

    };
}

handle_request!(handle_rtt, call_rtt_http);
handle_request!(handle_rtb, call_rtb_http);
handle_request_with_ws!(handle_rtt_ws, call_rtt_http, call_rtt_ws);
handle_request_with_ws!(handle_rtb_ws, call_rtb_http, call_rtb_ws);
