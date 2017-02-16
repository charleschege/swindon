_VARS:
  - &LISTEN ${listen_address}
  - &DEBUG_ROUTING ${debug_routing}
  - &PROXY_ADDRESS ${proxy_address}
  - &SPOOL_ADDRESS ${spool_address}

listen:
- *LISTEN

# These are defaults
#
# max_connections: 1000
# pipeline_depth: 2
# listen_error_timeout: 100ms

server_name: swindon/func-tests
debug-routing: *DEBUG_ROUTING

# Configure all possible routing?
routing:

  ### !EmptyGif routes ###
  localhost/empty.gif: empty_gif
  localhost/empty-w-headers.gif: empty_gif_w_headers
  localhost/empty-w-content-length.gif: empty_gif_w_clen

  ### !SingleFile routes ###
  localhost/static-file: single_file
  localhost/missing-file: missing_file
  localhost/no-permission: no-permission

  ### !Static routes ###
  localhost/static: static
  localhost/static-w-headers: static_w_headers
  localhost/static-w-ctype: static_w_ctype

  # TODO: add overlapping routes:
  #   /static: !Proxy & /static/file: !SingleFile

  ### !Proxy routes ###
  localhost/proxy: proxy
  localhost/proxy-w-prefix: proxy_w_prefix

  ### !SwindonChat routes ###
  localhost/swindon-chat: swindon_chat

  ### !WebsocketEcho routes ###
  localhost/websocket-echo: websocket_echo

# Configure all possible handlers?
handlers:
  # Allowed handlers are: SwindonChat, Static, SingleFile, Proxy,
  #   EmptyGif, HttpBin, WebsocketEcho;

  ### EmptyGif handlers ###
  empty_gif: !EmptyGif
  empty_gif_w_headers: !EmptyGif
    extra-headers:
      X-Some-Header: some value
  empty_gif_w_clen: !EmptyGif
    extra-headers:
      Content-Type: image/other
      Content-Length: 100500

  ### SingleFile handlers ###

  single_file: !SingleFile
    path: /work/tests/assets/static_file.txt
    content-type: text/plain
  missing_file: !SingleFile
    path: /work/tests/assets/missing_file.txt
    content-type: text/is/missing
  no_permission: !SingleFile
    path: /work/tests/assets/permission.txt
    content-type: text/no/permission

  ### Static handlers ###

  static: !Static
    path: /work/tests/assets/
  static_w_headers: !Static
    path: /work/tests/assets/
    extra-headers:
      X-Some-Header: some value
  static_w_ctype: !Static
    path: /work/tests/assets/
    extra-headers:
      Content-Type: something/other

  ### Proxy handlers ###

  proxy: !Proxy
    destination: proxy_dest/
  proxy_w_prefix: !Proxy
    destination: proxy_dest/prefix

  ### SwindonChat handlers ###
  swindon_chat: !SwindonChat
    session_pool: swindon_pool
    http_route: swindon_http_dest
    message_handlers:
      "*": swindon_chat_dest/

  ### WebsocketEcho handlers ###
  websocket_echo: !WebsocketEcho

session-pools:
  swindon_pool:
    listen:
    - *SPOOL_ADDRESS
    inactivity_handlers:
    - swindon_chat_dest/
    ### defaults: ###
    # pipeline_depth: 2
    # max_connections: 1000
    # listen_error_timeout: 100ms
    # max_payload_size: 10485760
    # inactivity:
    #   new_connection: 60s
    #   client_min: 1s
    #   client_max: 2h
    #   client_default: 1s

http-destinations:
  ### Proxy destintations ###
  proxy_dest:
    addresses:
    - *PROXY_ADDRESS

  ### SwindonChat destinations ###
  swindon_http_dest:
    addresses:
    - *PROXY_ADDRESS
  swindon_chat_dest:
    addresses:
    - *PROXY_ADDRESS
