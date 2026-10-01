import os

import uvicorn

if __name__ == "__main__":
    uvicorn.run("murmur_bridge.server:app", host="127.0.0.1", port=int(os.environ.get("MURMUR_HTTP_PORT", 8765)), log_level="warning")
