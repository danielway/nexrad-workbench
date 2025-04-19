# NEXRAD Workbench

## Desktop

```
cargo run --bin nexrad-workbench
```

## WASM

```
wasm-pack build --target web --out-dir web/pkg --out-name nexrad_workbench
basic-http-server --addr 0.0.0.0:8080 ./web
```