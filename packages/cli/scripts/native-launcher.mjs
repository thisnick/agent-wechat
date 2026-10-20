#!/usr/bin/env node
import { resolveNative, runNative } from './native-runtime.mjs';
try { runNative(await resolveNative(), process.argv.slice(2)); }
catch (error) { console.error(error.message); process.exitCode = 1; }
