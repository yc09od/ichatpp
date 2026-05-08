/** @type {import('next').NextConfig} */
const nextConfig = {
  // Standalone output produces a self-contained .next/standalone bundle
  // with only the runtime files Next needs — bypass the heavy
  // node_modules tree in the runner image. Pairs with Dockerfile.frontend.
  output: 'standalone',
};

export default nextConfig;
