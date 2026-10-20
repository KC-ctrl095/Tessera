// RPC answers belong to the connection that sent them, not a later server
// (or a fresh connection to the same server).
import { backend } from './backend';
let generation = 0;
backend.onStatus(() => generation++);
export const connectionGeneration = () => generation;
