// Grafana-native HTTP regression + load tests for the deployed jump-cannon
// graph-api. Runs nightly from the k6 CronJob (charts/jump-cannon values →
// tests.k6) and streams results to the in-cluster Prometheus with the
// experimental prometheus-remote-write output, so Grafana sees each run's
// checks, request latencies, and error rates as `k6_*` series labeled
// app="jump-cannon", test="k6-api-smoke", and one testid per CronJob run.
//
// Two scenarios:
//   - smoke: one iteration of the full endpoint sweep (every GET the
//     frontend's boot path exercises) with per-endpoint checks. Catches
//     "an endpoint 500'd" regressions.
//   - load: sustained read traffic against the hot endpoints for
//     K6_LOAD_DURATION (default 30s) at K6_LOAD_VUS (default 5). Surfaces
//     p95/p99 drift and error-rate regressions under concurrency.
//
// Per-endpoint `name` tags become Prometheus labels (k6_http_req_duration_p95
// {name="search"} …), so the dashboard and alerting can threshold each route.
import http from 'k6/http';
import { check, sleep } from 'k6';

const BASE = __ENV.JUMP_CANNON_BASE_URL || 'http://jump-cannon:80';
const LOAD_DURATION = __ENV.K6_LOAD_DURATION || '30s';
const LOAD_VUS = parseInt(__ENV.K6_LOAD_VUS || '5', 10);

// Run-wide tags become Prometheus series labels on every k6_* metric.
export const options = {
  tags: {
    app: 'jump-cannon',
    test: 'k6-api-smoke',
    testid: __ENV.JUMP_CANNON_RUN_ID || 'local',
  },
  scenarios: {
    smoke: {
      executor: 'per-vu-iterations',
      vus: 1,
      iterations: 1,
      exec: 'smoke',
    },
    load: {
      executor: 'constant-vus',
      vus: LOAD_VUS,
      duration: LOAD_DURATION,
      startTime: LOAD_DURATION === '0s' ? undefined : '10s',
      exec: 'load',
    },
  },
  thresholds: {
    'http_req_failed': ['rate<0.01'],
    'http_req_duration{name:schema}': ['p(95)<2000'],
    'http_req_duration{name:search}': ['p(95)<2000'],
    'http_req_duration{name:positions}': ['p(95)<5000'],
    'http_req_duration{name:edges}': ['p(95)<5000'],
    'http_req_duration{name:csr}': ['p(95)<5000'],
    'http_req_duration{name:pagerank}': ['p(95)<5000'],
    'http_req_duration{name:node_meta}': ['p(95)<1000'],
    // Load scenario: the hot read path must stay well under the smoke
    // budgets under concurrency too.
    'http_req_duration{scenario:load}': ['p(95)<5000'],
  },
};

// Minimal protobuf decode for SearchResults{ ids: string = 1, total: u32 = 2 }
// (crates/graph-api/proto/graph.proto). We only need the first repeated
// string field: tag 0x0a (field 1, wire type 2), varint length, utf-8 id.
function firstSearchId(buf) {
  const bytes = new Uint8Array(buf);
  let i = 0;
  while (i < bytes.length) {
    const tag = bytes[i++];
    if (tag === 0x0a) {
      // varint length
      let len = 0;
      let shift = 0;
      while (true) {
        const b = bytes[i++];
        len |= (b & 0x7f) << shift;
        if ((b & 0x80) === 0) break;
        shift += 7;
      }
      return new TextDecoder().decode(bytes.subarray(i, i + len));
    }
    // skip other fields (only varint (0) / len-delimited (2) appear here)
    const wireType = tag & 0x7;
    if (wireType === 0) {
      while (bytes[i++] & 0x80) {}
    } else if (wireType === 2) {
      let len = 0;
      let shift = 0;
      while (true) {
        const b = bytes[i++];
        len |= (b & 0x7f) << shift;
        if ((b & 0x80) === 0) break;
        shift += 7;
      }
      i += len;
    } else {
      return null;
    }
  }
  return null;
}

// The frontend's boot-path sweep: every GET the app issues while opening a
// graph. One named request per endpoint so checks and latency panels
// attribute failures to a route, not to "the smoke".
export function smoke() {
  // Structured JSON endpoints.
  const importers = http.get(`${BASE}/importers`, { tags: { name: 'importers' } });
  check(importers, { 'importers 200': (r) => r.status === 200 });

  const schema = http.get(`${BASE}/graph/schema`, { tags: { name: 'schema' } });
  check(schema, {
    'schema 200': (r) => r.status === 200,
    'schema body': (r) => r.body.length > 0,
  });

  const compute = http.get(`${BASE}/compute/health`, { tags: { name: 'compute-health' } });
  check(compute, { 'compute health 200': (r) => r.status === 200 });

  const engines = http.get(`${BASE}/compute/engines`, { tags: { name: 'compute-engines' } });
  check(engines, { 'compute engines 200': (r) => r.status === 200 });

  const progress = http.get(`${BASE}/progress?since=0`, { tags: { name: 'progress' } });
  check(progress, { 'progress 200': (r) => r.status === 200 });

  // Search — the frontend's primary query path (protobuf). responseType
  // binary: k6 otherwise hands r.body over as a latin1 string and the
  // protobuf decoder below sees garbage bytes.
  const search = http.get(`${BASE}/search?q=memory`, {
    responseType: 'binary',
    tags: { name: 'search' },
  });
  check(search, {
    'search 200': (r) => r.status === 200,
    // responseType binary hands r.body over as an ArrayBuffer.
    'search body': (r) => r.body.byteLength > 0,
  });

  const csr = http.get(`${BASE}/graph/csr.bin`, { tags: { name: 'csr' } });
  check(csr, { 'csr 200': (r) => r.status === 200 });

  const summary = http.get(`${BASE}/graph/meta_summary`, { tags: { name: 'meta_summary' } });
  check(summary, { 'meta summary 200': (r) => r.status === 200 });

  // Precomputed graph metrics (binary f32/u32 buffers).
  const pagerank = http.get(`${BASE}/graph/metrics/pagerank`, { tags: { name: 'pagerank' } });
  check(pagerank, { 'pagerank 200': (r) => r.status === 200 });

  const degree = http.get(`${BASE}/graph/metrics/degree`, { tags: { name: 'degree' } });
  check(degree, { 'degree 200': (r) => r.status === 200 });

  // Per-node metadata: resolve a real node id from the search result and
  // fetch its NodeMeta. Node ids are URL-path segments (wildcard route), so
  // encodeURIComponent keeps multi-segment vault ids intact.
  if (search.status === 200) {
    const nodeId = firstSearchId(search.body);
    if (nodeId) {
      const node = http.get(`${BASE}/node/${encodeURIComponent(nodeId)}`, {
        tags: { name: 'node_meta' },
      });
      check(node, { 'node meta 200': (r) => r.status === 200 });
    }
  }
}

// Sustained read load on the hot endpoints (what the open dashboard polls
// and re-fetches on snapshot revisions). Iteration sleep keeps per-VU rps
// realistic rather than a tight loop.
export function load() {
  http.get(`${BASE}/graph/schema`, { tags: { name: 'schema' } });
  http.get(`${BASE}/search?q=memory`, { tags: { name: 'search' } });
  http.get(`${BASE}/graph/positions`, { tags: { name: 'positions' } });
  http.get(`${BASE}/progress?since=0`, { tags: { name: 'progress' } });
  sleep(1);
}
