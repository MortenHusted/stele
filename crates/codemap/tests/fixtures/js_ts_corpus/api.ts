import { jsLeaf as leaf } from "./leaf.js";
import type { Shape } from "./types";
export type { Shape } from "./types";
export { jsLeaf } from "./leaf.js";
export interface Shape { area(): number; }
export type ID = string;
export enum Mode { Small, Large }
export namespace Tools { export function helper() { return leaf(); } }
export function overload(value: number): number;
export function overload(value: string): string;
export function overload(value: number | string) { return value; }
export class Box {
  field = leaf();
  constructor() { leaf(); }
  method() { return leaf(); }
}
export function outer(value = leaf()) {
  function inner() { return leaf(); }
  const nested = () => leaf();
  return [inner, nested];
}
