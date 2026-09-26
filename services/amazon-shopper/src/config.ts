import os from 'node:os';
import path from 'node:path';

export const config = {
  host: process.env.SHOPPER_HOST || '127.0.0.1',
  port: Number(process.env.SHOPPER_PORT || 4100),
  token: process.env.SHOPPER_TOKEN || '',
  profileDir:
    process.env.SHOPPER_PROFILE_DIR || path.join(os.homedir(), '.vox-shopper', 'profile'),
  // Safe by default: stop on a highlighted "Place your order" unless explicitly disabled.
  dryRun: (process.env.SHOPPER_DRY_RUN ?? 'true').trim().toLowerCase() !== 'false',
  baseUrl: 'https://www.amazon.in',
};
