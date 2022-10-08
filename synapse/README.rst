==============================================
Matrix Synapse module which implements MSC3886
==============================================

Usage::

    pip install matrix-http-rendezvous-synapse

In your homeserver.yaml::

    modules:
      - module: matrix_http_rendezvous_synapse.SynapseRendezvousModule
        config:
          prefix: /rendezvous
