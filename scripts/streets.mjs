#!/usr/bin/env node
// Takes the streets of Kyiv out of the extract of OpenStreetMap that the OSRM server
// is prepared from, as points that demo orders are put on (D87).
//
//   node scripts/streets.mjs                       osrm/kyiv.osm.pbf
//   node scripts/streets.mjs path/to/other.osm.pbf
//
// scripts/osrm-prepare fetches the extract. This reads it and writes
// crates/routeforge-api/data/kyiv-streets.bin, which the API compiles in. Run it again
// whenever newer streets are wanted, and commit what it writes: the same extract gives
// the same file, byte for byte, and another one gives demo orders elsewhere for the
// same seed.
//
// A street is a way a car may stop in for a parcel: `highway` is primary, secondary,
// tertiary, unclassified, residential or living_street. Not a bridge or a tunnel, not an
// area, and not private. Along each one there is a point every SPACING_M metres.
//
// The file is the south-west corner of the extract, as two little-endian i32 in
// hundred-thousandths of a degree, latitude first. Then each point, as two
// little-endian u16 in the same unit north and east of that corner. That is about a
// metre, and holds an extract of up to 0.655 degrees each way.
//
// It needs Node 22 or later and no package: Node's own zlib inflates the extract.
//
// The data is (c) OpenStreetMap contributors, under the Open Database License:
// https://www.openstreetmap.org/copyright

import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { inflateSync } from 'node:zlib';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const extract = process.argv[2] ?? join(root, 'osrm', 'kyiv.osm.pbf');
const output = join(root, 'crates', 'routeforge-api', 'data', 'kyiv-streets.bin');

const STREETS = new Set([
  'primary',
  'secondary',
  'tertiary',
  'unclassified',
  'residential',
  'living_street',
]);
const SPACING_M = 25;
/** The unit of the file, a hundred-thousandth of a degree. */
const STEPS = 1e5;
/** As `routeforge_domain::EARTH_RADIUS_M`. */
const EARTH_RADIUS_M = 6_371_008.8;
const METRES_A_DEGREE = (EARTH_RADIUS_M * Math.PI) / 180;

/** What went wrong, in words for whoever runs the script. */
class Failure extends Error {}

// ------------------------------------------------------------------ protocol buffers

/** Reads the fields of one protocol buffer message, front to back. */
class Message {
  constructor(bytes) {
    this.bytes = bytes;
    this.at = 0;
  }

  get done() {
    return this.at >= this.bytes.length;
  }

  /**
   * A varint, summed by multiplying so that it stays exact as a Number up to 2^53. An
   * id of OpenStreetMap is far below that.
   */
  varint() {
    let value = 0;
    let scale = 1;
    for (;;) {
      const byte = this.bytes[this.at++];
      if (byte === undefined) throw new Failure('the extract ends inside a number');
      value += (byte & 0x7f) * scale;
      if ((byte & 0x80) === 0) return value;
      scale *= 128;
    }
  }

  /** A signed varint that is not zigzagged, as an `int64` of a negative value is. */
  int64() {
    const value = this.varint();
    return value >= 2 ** 63 ? value - 2 ** 64 : value;
  }

  sint() {
    return zigzag(this.varint());
  }

  /** The number and the wire type of the next field. */
  key() {
    const key = this.varint();
    return [Math.floor(key / 8), key % 8];
  }

  bytesField() {
    const length = this.varint();
    const start = this.at;
    this.at += length;
    if (this.at > this.bytes.length) throw new Failure('the extract ends inside a field');
    return this.bytes.subarray(start, this.at);
  }

  /** Passes over a field that is not wanted. */
  skip(wire) {
    if (wire === 0) this.varint();
    else if (wire === 1) this.at += 8;
    else if (wire === 2) this.bytesField();
    else if (wire === 5) this.at += 4;
    else throw new Failure(`the extract has a field of wire type ${wire}`);
  }

  /** A packed field of varints, each read by `read`. */
  packed(read = (message) => message.varint()) {
    const inner = new Message(this.bytesField());
    const values = [];
    while (!inner.done) values.push(read(inner));
    return values;
  }

  /** A packed field of zigzagged varints, each the difference from the one before. */
  deltas() {
    let sum = 0;
    return this.packed((inner) => (sum += inner.sint()));
  }
}

function zigzag(value) {
  return value % 2 === 0 ? value / 2 : -(value + 1) / 2;
}

// --------------------------------------------------------------------- the extract

/** Each block of the extract, by its type, inflated. */
function* blocks(file) {
  let at = 0;
  while (at < file.length) {
    const headerLength = file.readUInt32BE(at);
    at += 4;
    const header = new Message(file.subarray(at, at + headerLength));
    at += headerLength;
    let type = '';
    let size = 0;
    while (!header.done) {
      const [field, wire] = header.key();
      if (field === 1) type = new TextDecoder().decode(header.bytesField());
      else if (field === 3) size = header.varint();
      else header.skip(wire);
    }
    const blob = new Message(file.subarray(at, at + size));
    at += size;
    let data = null;
    while (!blob.done) {
      const [field, wire] = blob.key();
      if (field === 1) data = blob.bytesField();
      else if (field === 3) data = inflateSync(blob.bytesField());
      else if (field === 2) blob.varint();
      else throw new Failure(`a block of the extract is compressed in a way zlib is not (field ${field})`);
    }
    if (data === null) throw new Failure('a block of the extract holds nothing');
    yield [type, data];
  }
}

/** The rectangle the extract was cut to, in degrees, if its header names one. */
function frameOf(header) {
  const message = new Message(header);
  while (!message.done) {
    const [field, wire] = message.key();
    if (field !== 1) {
      message.skip(wire);
      continue;
    }
    const box = new Message(message.bytesField());
    const sides = {};
    while (!box.done) {
      const [side, sideWire] = box.key();
      const name = ['', 'west', 'east', 'north', 'south'][side];
      if (name) sides[name] = box.sint() / 1e9;
      else box.skip(sideWire);
    }
    return sides;
  }
  return null;
}

/** What a block of data says about itself: its strings, and where its points start. */
function readBlock(data, eachGroup) {
  const message = new Message(data);
  const block = { strings: [], granularity: 100, latOffset: 0, lonOffset: 0 };
  const groups = [];
  while (!message.done) {
    const [field, wire] = message.key();
    if (field === 1) {
      const table = new Message(message.bytesField());
      const decoder = new TextDecoder();
      while (!table.done) {
        const [entry, entryWire] = table.key();
        if (entry === 1) block.strings.push(decoder.decode(table.bytesField()));
        else table.skip(entryWire);
      }
    } else if (field === 2) groups.push(message.bytesField());
    else if (field === 17) block.granularity = message.varint();
    else if (field === 19) block.latOffset = message.int64();
    else if (field === 20) block.lonOffset = message.int64();
    else message.skip(wire);
  }
  // The strings and the offsets may come after the groups, so the groups wait.
  for (const group of groups) eachGroup(new Message(group), block);
}

/** The streets among the ways of a group: the ids of their nodes, in order. */
function streetsOf(group, block, streets) {
  while (!group.done) {
    const [field, wire] = group.key();
    if (field !== 3) {
      group.skip(wire);
      continue;
    }
    const way = new Message(group.bytesField());
    let keys = [];
    let values = [];
    let refs = [];
    while (!way.done) {
      const [wayField, wayWire] = way.key();
      if (wayField === 2) keys = way.packed();
      else if (wayField === 3) values = way.packed();
      else if (wayField === 8) refs = way.deltas();
      else way.skip(wayWire);
    }
    const tags = {};
    keys.forEach((key, at) => (tags[block.strings[key]] = block.strings[values[at]]));
    if (isStreet(tags) && refs.length > 1) streets.push(refs);
  }
}

function isStreet(tags) {
  const set = (value) => value !== undefined && value !== 'no';
  return (
    STREETS.has(tags.highway) &&
    !set(tags.bridge) &&
    !set(tags.tunnel) &&
    tags.area !== 'yes' &&
    tags.access !== 'no' &&
    tags.access !== 'private'
  );
}

/** Where the wanted nodes of a group are, into `places`. */
function placesOf(group, block, places) {
  const degrees = (offset, value) => (offset + block.granularity * value) / 1e9;
  const keep = (id, lat, lon) => {
    if (places.has(id)) places.set(id, [degrees(block.latOffset, lat), degrees(block.lonOffset, lon)]);
  };
  while (!group.done) {
    const [field, wire] = group.key();
    if (field === 1) {
      const node = new Message(group.bytesField());
      let id = 0;
      let lat = 0;
      let lon = 0;
      while (!node.done) {
        const [nodeField, nodeWire] = node.key();
        if (nodeField === 1) id = node.sint();
        else if (nodeField === 8) lat = node.sint();
        else if (nodeField === 9) lon = node.sint();
        else node.skip(nodeWire);
      }
      keep(id, lat, lon);
    } else if (field === 2) {
      const dense = new Message(group.bytesField());
      let ids = [];
      let lats = [];
      let lons = [];
      while (!dense.done) {
        const [denseField, denseWire] = dense.key();
        if (denseField === 1) ids = dense.deltas();
        else if (denseField === 8) lats = dense.deltas();
        else if (denseField === 9) lons = dense.deltas();
        else dense.skip(denseWire);
      }
      ids.forEach((id, at) => keep(id, lats[at], lons[at]));
    } else group.skip(wire);
  }
}

// ---------------------------------------------------------------------- the points

/**
 * A point every SPACING_M metres along a street, from its first node. A node the
 * extract does not have breaks the street in two.
 */
function alongStreet(refs, places, emit) {
  let toNext = 0;
  let metres = 0;
  for (let at = 1; at < refs.length; at += 1) {
    const from = places.get(refs[at - 1]);
    const to = places.get(refs[at]);
    if (!from || !to) {
      toNext = 0;
      continue;
    }
    const east = (to[1] - from[1]) * METRES_A_DEGREE * Math.cos((((from[0] + to[0]) / 2) * Math.PI) / 180);
    const north = (to[0] - from[0]) * METRES_A_DEGREE;
    const length = Math.hypot(east, north);
    if (length === 0) continue;
    while (toNext <= length) {
      const share = toNext / length;
      emit(from[0] + (to[0] - from[0]) * share, from[1] + (to[1] - from[1]) * share);
      toNext += SPACING_M;
    }
    toNext -= length;
    metres += length;
  }
  return metres;
}

function main() {
  if (!existsSync(extract)) {
    throw new Failure(`there is no ${extract}. scripts/osrm-prepare fetches it.`);
  }
  const file = readFileSync(extract);

  // First the ways, to know which nodes are wanted: an extract has its nodes first.
  let frame = null;
  const streets = [];
  for (const [type, data] of blocks(file)) {
    if (type === 'OSMHeader') frame = frameOf(data);
    else if (type === 'OSMData') readBlock(data, (group, block) => streetsOf(group, block, streets));
  }
  const places = new Map();
  for (const refs of streets) for (const ref of refs) places.set(ref, null);
  for (const [type, data] of blocks(file)) {
    if (type === 'OSMData') readBlock(data, (group, block) => placesOf(group, block, places));
  }
  if (frame === null || [frame.south, frame.west, frame.north, frame.east].includes(undefined)) {
    throw new Failure('the header of the extract names no rectangle to frame the points in');
  }

  const south = Math.floor(frame.south * STEPS);
  const west = Math.floor(frame.west * STEPS);
  const height = Math.ceil(frame.north * STEPS) - south;
  const width = Math.ceil(frame.east * STEPS) - west;
  if (height > 0xffff || width > 0xffff) {
    throw new Failure('the extract is more than 0.655 degrees across, which the file cannot hold');
  }
  // A point is its two steps in one number, so that a set finds the same point twice.
  // A street that leaves the rectangle is cut at its edge: the extract has the ways that
  // cross it, and not the streets round them.
  const points = new Set();
  let metres = 0;
  for (const refs of streets) {
    metres += alongStreet(refs, places, (lat, lon) => {
      const north = Math.round(lat * STEPS) - south;
      const east = Math.round(lon * STEPS) - west;
      if (north >= 0 && north <= height && east >= 0 && east <= width) points.add(north * 0x10000 + east);
    });
  }
  if (points.size === 0) throw new Failure('the extract has no streets');

  const sorted = Uint32Array.from(points).sort();
  const bytes = Buffer.alloc(8 + 4 * sorted.length);
  bytes.writeInt32LE(south, 0);
  bytes.writeInt32LE(west, 4);
  sorted.forEach((point, at) => {
    bytes.writeUInt16LE(Math.floor(point / 0x10000), 8 + 4 * at);
    bytes.writeUInt16LE(point % 0x10000, 10 + 4 * at);
  });
  writeFileSync(output, bytes);
  console.log(
    `${streets.length} streets, ${Math.round(metres / 1000)} km of them, ` +
      `from ${frame.south} to ${frame.north} north and ${frame.west} to ${frame.east} east: ` +
      `${sorted.length} points, ${Math.round(bytes.length / 1024)} kB, in ${output}`,
  );
}

try {
  main();
} catch (error) {
  if (!(error instanceof Failure)) throw error;
  console.error(`streets: ${error.message}`);
  process.exitCode = 1;
}
