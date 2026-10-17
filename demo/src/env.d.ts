/// <reference types="vite/client" />

declare module 'virtual:charts-data' {
  import type { ChartsData } from './charts-schema';
  const data: ChartsData;
  export default data;
}
