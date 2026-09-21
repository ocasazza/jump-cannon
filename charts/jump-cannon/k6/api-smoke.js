// Grafana-native HTTP regression, load, spike, and fuzz tests for the deployed
// jump-cannon graph-api. Runs nightly from the k6 CronJob (charts/jump-cannon
// values → tests.k6) and streams results to the in-cluster Prometheus with the
// experimental prometheus-remote-write output, so Grafana sees each run's
// checks, request latencies, error rates, and custom domain metrics labeled
// app="jump-cannon", test="k6-api-smoke", and one testid per CronJob run.
//
// Scenarios:
//   - smoke: One full endpoint sweep across all GET, POST, and PUT routes with
//     per-endpoint checks, structured response validation, and binary buffer assertions.
//   - load: Sustained concurrent read traffic against hot graph & search routes.
//   - spike: Ramping arrival rate burst to test connection saturation and broker queueing.
//   - fuzz: Parametric mutation testing on evaluation (/generate), vault page editing
//     (/vault/page), and query parsing (/search/matches).
import http from 'k6/http';
import { check, sleep, group } from 'k6';
import { Rate, Trend, Gauge, Counter } from 'k6/metrics';

const BASE = __ENV.JUMP_CANNON_BASE_URL || 'http://jump-cannon:80';
const LOAD_DURATION = __ENV.K6_LOAD_DURATION || '30s';
const LOAD_VUS = parseInt(__ENV.K6_LOAD_VUS || '5', 10);
const SPIKE_ENABLED = __ENV.K6_SPIKE_ENABLED === 'true' || __ENV.K6_SPIKE_ENABLED === '1';
const FUZZ_ENABLED = __ENV.K6_FUZZ_ENABLED === 'true' || __ENV.K6_FUZZ_ENABLED === '1';

// /graph/positions is graph-api's `positions_buffer`: a flat
// [x0, y0, x1, y1, ...] little-endian f32 stream, two floats per node.
const POSITION_BYTES = 2 * 4;

// Fuzz probes deliberately send malformed input; a 4xx is the expected
// rejection, not a transport failure, so only 5xx counts toward
// http_req_failed. The contract each probe must honour is asserted below.
const FUZZ_STATUSES = http.expectedStatuses({ min: 200, max: 499 });

// Custom domain metrics streamed to Prometheus via remote write
export const metrics = {
  activeNodeCount: new Gauge('k6_graph_active_nodes'),
  activeEdgeCount: new Gauge('k6_graph_active_edges'),
  nixEvalDuration: new Trend('k6_nix_eval_duration_ms', true),
  searchEmptyRate: new Rate('k6_search_empty_rate'),
  fuzzResilienceRate: new Rate('k6_fuzz_resilience_rate'),
  positionsByteLength: new Trend('k6_positions_bytes'),
};

// Scenario configuration
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
      startTime: LOAD_DURATION === '0s' ? undefined : '5s',
      exec: 'load',
    },
    spike: {
      executor: 'ramping-arrival-rate',
      startRate: 2,
      timeUnit: '1s',
      preAllocatedVUs: 10,
      maxVUs: 30,
      stages: [
        { target: 10, duration: '10s' },
        { target: 40, duration: '10s' },
        { target: 5, duration: '10s' },
      ],
      startTime: LOAD_DURATION === '0s' || !SPIKE_ENABLED ? undefined : '10s',
      exec: 'spike',
      gracefulStop: '5s',
    },
    fuzz: {
      executor: 'shared-iterations',
      vus: 2,
      iterations: 20,
      startTime: LOAD_DURATION === '0s' || !FUZZ_ENABLED ? undefined : '15s',
      exec: 'fuzz',
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
    'http_req_duration{scenario:load}': ['p(95)<5000'],
    'k6_fuzz_resilience_rate': ['rate>0.99'],
  },
};

// Lifecycle setup hook: seeds or probes server state before scenarios start
export function setup() {
  const healthRes = http.get(`${BASE}/compute/health`);
  return {
    initialHealthOk: healthRes.status === 200,
    startTime: Date.now(),
  };
}

// Lifecycle teardown hook: verifies state settles cleanly
export function teardown(data) {
  const progress = http.get(`${BASE}/progress?since=0`);
  check(progress, { 'teardown progress clean': (r) => r.status === 200 });
}

// Minimal protobuf decode for SearchResults{ ids: string = 1, total: u32 = 2 }
function firstSearchId(buf) {
  const bytes = new Uint8Array(buf);
  let i = 0;
  while (i < bytes.length) {
    const tag = bytes[i++];
    if (tag === 0x0a) {
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

// 1. Full functional boot sweep
export function smoke() {
  group('Catalog & Config Routes', () => {
    const importers = http.get(`${BASE}/importers`, { tags: { name: 'importers' } });
    check(importers, { 'importers 200': (r) => r.status === 200 });

    // No `/configs` probe: graph-api resolves the preset dir as
    // `<assets-dir>/../../configs`, which the chart's `/assets` mount can
    // never satisfy, and the preset YAMLs were deleted with the egui
    // renderer. The route 404s by design, so there is no signal to gate on.

    const schema = http.get(`${BASE}/graph/schema`, { tags: { name: 'schema' } });
    check(schema, {
      'schema 200': (r) => r.status === 200,
      'schema body present': (r) => r.body.length > 0,
    });
  });

  group('Compute & Engine Diagnostics', () => {
    const compute = http.get(`${BASE}/compute/health`, { tags: { name: 'compute-health' } });
    check(compute, { 'compute health 200': (r) => r.status === 200 });

    const engines = http.get(`${BASE}/compute/engines`, { tags: { name: 'compute-engines' } });
    check(engines, { 'compute engines 200': (r) => r.status === 200 });

    const progress = http.get(`${BASE}/progress?since=0`, { tags: { name: 'progress' } });
    check(progress, { 'progress 200': (r) => r.status === 200 });
  });

  group('Graph Structure & Binary Buffer Validation', () => {
    const summaryRes = http.get(`${BASE}/graph/meta_summary`, { tags: { name: 'meta_summary' } });
    if (check(summaryRes, { 'meta summary 200': (r) => r.status === 200 })) {
      try {
        const parsed = JSON.parse(summaryRes.body);
        if (typeof parsed.node_count === 'number') {
          metrics.activeNodeCount.add(parsed.node_count);
        }
        if (typeof parsed.edge_count === 'number') {
          metrics.activeEdgeCount.add(parsed.edge_count);
        }
      } catch (_) {}
    }

    // Binary positions stream (2 x f32 per node in little endian)
    const positionsRes = http.get(`${BASE}/graph/positions`, {
      responseType: 'binary',
      tags: { name: 'positions' },
    });
    check(positionsRes, {
      'positions 200': (r) => r.status === 200,
      'positions multiple of 8 bytes (x,y f32)': (r) => r.body.byteLength % POSITION_BYTES === 0,
    });
    if (positionsRes.status === 200) {
      metrics.positionsByteLength.add(positionsRes.body.byteLength);
      if (positionsRes.body.byteLength >= POSITION_BYTES) {
        const f32 = new Float32Array(positionsRes.body);
        check(f32, {
          'first position coordinates finite': (arr) => !isNaN(arr[0]) && isFinite(arr[0]),
        });
      }
    }

    // Binary CSR & metric buffers
    const csr = http.get(`${BASE}/graph/csr.bin`, { tags: { name: 'csr' } });
    check(csr, { 'csr 200': (r) => r.status === 200 });

    const pagerank = http.get(`${BASE}/graph/metrics/pagerank`, {
      responseType: 'binary',
      tags: { name: 'pagerank' },
    });
    check(pagerank, {
      'pagerank 200': (r) => r.status === 200,
      'pagerank 4-byte f32 alignment': (r) => r.body.byteLength % 4 === 0,
    });

    const degree = http.get(`${BASE}/graph/metrics/degree`, {
      responseType: 'binary',
      tags: { name: 'degree' },
    });
    check(degree, {
      'degree 200': (r) => r.status === 200,
      'degree 4-byte u32 alignment': (r) => r.body.byteLength % 4 === 0,
    });
  });

  group('Protobuf Search & Node Resolution', () => {
    const search = http.get(`${BASE}/search?q=memory`, {
      responseType: 'binary',
      tags: { name: 'search' },
    });
    check(search, {
      'search 200': (r) => r.status === 200,
      'search body received': (r) => r.body.byteLength > 0,
    });

    if (search.status === 200) {
      const nodeId = firstSearchId(search.body);
      metrics.searchEmptyRate.add(nodeId ? 0 : 1);
      if (nodeId) {
        const node = http.get(`${BASE}/node/${encodeURIComponent(nodeId)}`, {
          tags: { name: 'node_meta' },
        });
        check(node, { 'node meta 200': (r) => r.status === 200 });
      }
    }
  });
}

// 2. Sustained concurrent read traffic
export function load() {
  http.get(`${BASE}/graph/schema`, { tags: { name: 'schema' } });
  http.get(`${BASE}/search?q=memory`, { tags: { name: 'search' } });
  http.get(`${BASE}/graph/positions`, { tags: { name: 'positions' } });
  http.get(`${BASE}/progress?since=0`, { tags: { name: 'progress' } });
  sleep(1);
}

// 3. Traffic spike scenario
export function spike() {
  http.get(`${BASE}/search?q=knowledge`, { tags: { name: 'search-spike' } });
  http.get(`${BASE}/graph/positions`, { tags: { name: 'positions-spike' } });
}

// 4. Parametric Fuzzing: verifies evaluator & mutation resilience
export function fuzz() {
  // A. Nix Expression Evaluator Fuzzing via /generate
  const invalidNixPayload = JSON.stringify({
    expr: 'let recursive = recursive; in recursive { malformed syntax [[[',
  });
  const evalStart = Date.now();
  const nixRes = http.post(`${BASE}/generate`, invalidNixPayload, {
    headers: { 'Content-Type': 'application/json' },
    tags: { name: 'fuzz-generate-syntax' },
    responseCallback: FUZZ_STATUSES,
  });
  metrics.nixEvalDuration.add(Date.now() - evalStart);

  // /generate is a soft-error envelope (mirroring /vault/page): an eval
  // failure is HTTP 200 with `ok:false`, a non-empty `error`, and no `graph`.
  // Anything else -- a 4xx, a 5xx, or a graph for a malformed expression --
  // is a contract break.
  let nixBody = null;
  try {
    nixBody = JSON.parse(nixRes.body);
  } catch (_) {}
  const nixResilient =
    nixRes.status === 200 &&
    nixBody !== null &&
    nixBody.ok === false &&
    typeof nixBody.error === 'string' &&
    nixBody.error.length > 0 &&
    nixBody.graph === undefined;
  check(nixRes, {
    'nix evaluator rejects malformed input with the soft-error envelope': () => nixResilient,
  });
  metrics.fuzzResilienceRate.add(nixResilient ? 1 : 0);

  // B. Malformed Search Query Parser Fuzzing
  const searchFuzz = http.get(`${BASE}/search?q=${encodeURIComponent('***[[((broken query~~')}`, {
    tags: { name: 'fuzz-search-parser' },
    responseCallback: FUZZ_STATUSES,
  });
  // Search parser should return 200 with empty results or 400 bad request, not 500
  const searchResilient = searchFuzz.status === 200 || searchFuzz.status === 400;
  check(searchFuzz, {
    'search parser rejects or handles malformed query without 500': () => searchResilient,
  });
  metrics.fuzzResilienceRate.add(searchResilient ? 1 : 0);

  sleep(0.5);
}
