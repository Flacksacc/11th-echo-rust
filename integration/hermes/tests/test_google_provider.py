import unittest
from echo_bridge.google_provider import apply_request_parameters, RequestParametersHTTP


class GoogleRequestGate(unittest.TestCase):
    def test_generation_options_merge_without_replacing_agent_context(self):
        request = {"contents": [{"role": "user"}], "tools": [{"functionDeclarations": []}],
                   "generationConfig": {"maxOutputTokens": 512, "temperature": 0.5}}
        result = apply_request_parameters(request, {"generationConfig": {
            "thinkingConfig": {"thinkingLevel": "low"}, "temperature": 0.2}})
        self.assertEqual(result["generationConfig"]["maxOutputTokens"], 512)
        self.assertEqual(result["generationConfig"]["thinkingConfig"], {"thinkingLevel": "low"})
        self.assertEqual(result["contents"], request["contents"])
        self.assertEqual(result["tools"], request["tools"])
        self.assertEqual(request["generationConfig"]["temperature"], 0.5)

    def test_unknown_or_context_replacing_options_are_explicit_errors(self):
        for extra in ({"unsupportedSetting": True}, {"contents": []}, {"generationConfig": 1}):
            with self.assertRaises(ValueError):
                apply_request_parameters({}, extra)

    def test_streaming_and_nonstreaming_requests_receive_identical_options(self):
        class HTTP:
            def post(self, url, **kwargs):
                return kwargs["json"]
            def stream(self, method, url, **kwargs):
                return kwargs["json"]
        client = RequestParametersHTTP(HTTP(), {"generationConfig": {"seed": 123}})
        self.assertEqual(client.post("url", json={}), client.stream("POST", "url", json={}))
        self.assertEqual(client.post("url", json={})["generationConfig"]["seed"], 123)
