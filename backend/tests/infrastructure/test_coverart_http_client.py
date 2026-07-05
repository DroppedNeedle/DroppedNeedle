from infrastructure.http.client import (
    HttpClientFactory,
    get_coverart_http_client,
    get_http_client,
)


def test_coverart_client_uses_short_budget_and_distinct_name():
    """Covers ride their own short-budget client so a slow archive.org fetch degrades to a
    placeholder instead of holding the request open, and retuning it never touches the shared
    'default' client used by MusicBrainz et al."""
    client = get_coverart_http_client()

    # Short budget: 6s read, 3s connect - not the 10s shared default.
    assert client.timeout.read == 6.0
    assert client.timeout.connect == 3.0

    # Cached under its own name, and a different instance from the default client.
    assert HttpClientFactory._clients.get("coverart") is client
    assert client is not get_http_client()
