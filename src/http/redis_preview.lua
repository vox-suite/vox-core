local key = KEYS[1]
local kind = redis.call('TYPE', key).ok
local ttl = redis.call('TTL', key)
local size = 0
local value = false
local truncated = false
local budget = 65536
local function clip(v)
  if type(v) == 'string' then
    local length = math.min(#v, 2048, budget)
    budget = budget - length
    if length < #v then truncated = true end
    return string.sub(v, 1, length)
  elseif type(v) == 'table' then
    local result = {}
    for i = 1, math.min(#v, 80) do result[i] = clip(v[i]) end
    if #v > 80 then truncated = true end
    return result
  end
  return v
end
if kind == 'string' then
  size = redis.call('STRLEN', key)
  value = redis.call('GETRANGE', key, 0, 65535)
  truncated = size > 65536
elseif kind == 'list' then
  size = redis.call('LLEN', key)
  value = clip(redis.call('LRANGE', key, 0, 39))
  truncated = truncated or size > 40
elseif kind == 'hash' then
  size = redis.call('HLEN', key)
  value = clip(redis.call('HSCAN', key, 0, 'COUNT', 40)[2])
  truncated = truncated or size > (#value / 2)
elseif kind == 'set' then
  size = redis.call('SCARD', key)
  value = clip(redis.call('SSCAN', key, 0, 'COUNT', 40)[2])
  truncated = truncated or size > #value
elseif kind == 'zset' then
  size = redis.call('ZCARD', key)
  value = clip(redis.call('ZRANGE', key, 0, 39, 'WITHSCORES'))
  truncated = truncated or size > 40
elseif kind == 'stream' then
  size = redis.call('XLEN', key)
  value = clip(redis.call('XRANGE', key, '-', '+', 'COUNT', 20))
  truncated = truncated or size > 20
end
return {kind, ttl, size, truncated and 1 or 0, value}
