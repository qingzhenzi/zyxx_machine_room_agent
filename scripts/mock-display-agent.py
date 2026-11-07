import socket, json, threading, time, os, sys

sock_path = '/tmp/qe-mock-display.sock'
try: os.unlink(sock_path)
except FileNotFoundError: pass

server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
server.bind(sock_path); server.listen(16)

lock = threading.Lock()
state = {}

def mk(v, t, params):
    return json.dumps({"overall":"booting" if t != "idle" else "ready",
        "config_sufficient":True,
        "display":{"version":v,"template":t,"params":params},"phases":[]}).encode()

def rotate():
    v = 0
    seq = [("boot", {"message":"正在初始化系统服务","detail":"qe-agent v0.1.0"}),
           ("progress", {"message":"正在下载播放资源","detail":"资源包 3 / 10","progress":30}),
           ("enroll", {"state":"pending","detail":"设备已提交,等待房间管理员审批"}),
           ("idle", {})]
    i = 0
    while True:
        t, p = seq[i % 4]
        with lock:
            state["data"] = mk(v, t, p)
        v += 1; i += 1
        time.sleep(5)

def serve():
    while True:
        conn, _ = server.accept()
        try:
            conn.settimeout(2)
            conn.recv(4096)
            with lock:
                data = state.get("data", mk(0, "boot", {}))
            conn.sendall(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: " + str(len(data)).encode() + b"\r\n\r\n" + data)
        except Exception:
            pass
        finally:
            try: conn.close()
            except Exception: pass

threading.Thread(target=rotate, daemon=True).start()
threading.Thread(target=serve, daemon=True).start()
print("mock ready", flush=True)
while True:
    time.sleep(30)
