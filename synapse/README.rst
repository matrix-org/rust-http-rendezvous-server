Matrix Synapse module which implements MSC3886 Simple HTTP rendezvous server
============================================================================

This is an implementation of `MSC3886
<https://github.com/matrix-org/matrix-spec-proposals/pull/3886>`_ for `Synapse
<https://github.com/matrix-org/synapse>`_.

-----
Usage
-----

1. Install the module to make it available to your Synapse environment::

    pip install matrix-http-rendezvous-synapse

2. Enable the module in your homeserver.yaml::

    modules:
      - module: matrix_http_rendezvous_synapse.SynapseRendezvousModule
        config:
          prefix: /_synapse/client/org.matrix.msc3886/rendezvous

3. Make the module available at the actual API endpoint in the Client-Server API by adding this to your homeserver.yaml::

    experimental_features:
      msc3886_endpoint: /_synapse/client/org.matrix.msc3886/rendezvous

4. Enable additional CORS headers for the API endpoints within the listeners section of your homeserver.yaml::

    listeners:
      - type: http
        experimental_cors_msc3886: True
        # ... rest of the HTTP listener config

5. Run Synapse with the asyncio reactor enabled::

    SYNAPSE_ASYNC_IO_REACTOR=1 python -m synapse.app.homeserver

---------------------
Configuration options
---------------------

Apart from the ``prefix`` the following config options are available:

- ``ttl``: The time-to-live of the rendezvous session. Defaults to 60s.
- ``max_bytes``: The maximum number of bytes that can be sent in a single request. Defaults to 4096 bytes.
- ``max_entries``: The maximum number of entries to keep. Defaults to 10 000.

An example configuration setting these and a custom prefix would like::

    modules:
      - module: matrix_http_rendezvous_synapse.SynapseRendezvousModule
        config:
          prefix: /rendezvous
          ttl: 15s
          max_bytes: 10KiB
          max_entries: 50000

    experimental_features:
      msc3886_endpoint: /rendezvous # this should match above
    
    listeners:
      - type: http
        experimental_cors_msc3886: True
        # ... rest of the HTTP listener config

^^^^^^^^^^^^
Memory usage
^^^^^^^^^^^^

``max_entries`` and ``max_bytes`` allow to tune how much memory the module may take.
There is a constant overhead of approximately 1KiB per entry, so with the default config (``max_bytes = 4KiB``, ``max_entries = 10000``), the maximum theorical memory footprint of the module is ``(4KiB + ~1KiB) * 10000 ~= 50MiB``.
