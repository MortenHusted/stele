import { jsLeaf } from "./leaf.js";
export function forward() { return jsLeaf(1); } // site:js-import
export function overload(value: number): number;
export function overload(value: string): string;
export function overload(value: number | string) { return value; }
