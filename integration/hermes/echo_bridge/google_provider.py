"""Native Hermes provider extension for Echo's Google connection.

Named custom providers otherwise construct an OpenAI client even on Google's
native endpoint. Reuse the upstream Gemini profile and streaming client while
preserving Echo's provider-specific request parameters.
"""
from dataclasses import fields
import os
from pathlib import Path
import json


def apply_request_parameters(request, extra):
    """Overlay native request options without replacing agent messages/tools."""
    native = {"generationConfig", "safetySettings", "cachedContent", "labels"}
    adapter = {"thinking_config", "thinkingConfig", "response_format"}
    unsupported = set(extra) - native - adapter
    if unsupported:
        raise ValueError("Unsupported Gemini additional request parameters: " + ", ".join(sorted(unsupported)))
    result = dict(request)
    for key in native & extra.keys():
        value = extra[key]
        if key == "generationConfig":
            if not isinstance(value, dict):
                raise ValueError("Gemini generationConfig must be an object")
            result[key] = {**request.get(key, {}), **value}
        else:
            result[key] = value
    return result


class RequestParametersHTTP:
    """Keep upstream response/stream conversion, applying options on both paths."""
    def __init__(self, client, extra):
        self.client = client
        self.extra = extra

    def __getattr__(self, name):
        return getattr(self.client, name)

    def _options(self, kwargs):
        if isinstance(kwargs.get("json"), dict):
            kwargs = {**kwargs, "json": apply_request_parameters(kwargs["json"], self.extra)}
        return kwargs

    def post(self, url, **kwargs):
        return self.client.post(url, **self._options(kwargs))

    def stream(self, method, url, **kwargs):
        return self.client.stream(method, url, **self._options(kwargs))


def register():
    from providers import get_provider_profile, register_provider
    from agent.gemini_native_adapter import GeminiNativeClient

    path = Path(os.environ["HERMES_HOME"]).parent / "echo-config.json"
    if not path.is_file() or path.stat().st_size > 1024 * 1024:
        return
    supplied = json.loads(path.read_text(encoding="utf-8"))
    if supplied.get("model", {}).get("provider") != "echo-google":
        return
    upstream = get_provider_profile("gemini")
    if upstream is None:
        raise RuntimeError("The pinned Hermes Gemini provider is unavailable")
    entry = supplied["providers"]["echo-fast"]
    extra = entry.get("extra_body") or {}
    if not isinstance(extra, dict):
        raise ValueError("Gemini additional request parameters must be an object")
    apply_request_parameters({}, extra)

    class EchoGeminiClient(GeminiNativeClient):
        def __init__(self, **kwargs):
            super().__init__(**kwargs)
            self._http = RequestParametersHTTP(self._http, extra)

    class EchoGoogleProfile(type(upstream)):
        def create_client(self, **kwargs):
            return EchoGeminiClient(**{key: value for key, value in kwargs.items()
                if key in {"api_key", "base_url", "default_headers", "timeout", "http_client"}})

        def build_extra_body(self, **context):
            body = dict(super().build_extra_body(**context))
            # Explicit user parameters take precedence over inferred defaults.
            body.update(entry.get("extra_body") or {})
            return body

        def build_client_kwargs_extras(self, **context):
            from hermes_cli.config import _expand_env_vars
            result = dict(super().build_client_kwargs_extras(**context))
            if entry.get("extra_headers"):
                result["default_headers"] = _expand_env_vars(entry["extra_headers"])
            return result

    values = {field.name: getattr(upstream, field.name) for field in fields(upstream)}
    values.update(name="echo-google", aliases=(), env_vars=("ECHO_FAST_MODEL_KEY",),
                  base_url=entry["base_url"], display_name="Echo Google connection")
    register_provider(EchoGoogleProfile(**values))
