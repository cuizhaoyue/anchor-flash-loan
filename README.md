# Anchor Flash Loan

基于 Solana 和 Anchor 框架实现的闪电贷协议，使用指令内省（Instruction Introspection）技术确保借贷操作的原子性。

## 项目简介

闪电贷是一种无需抵押的借贷方式，借款和还款必须在同一个交易内原子完成。本协议通过以下机制实现安全性：

1. **原子性保证**：`borrow` 指令会检查同一交易的最后一条指令必须是 `repay`
2. **指令内省**：通过 Solana 的指令 sysvar 读取并验证交易中的其他指令
3. **费用收取**：固定 500 bps（5%）的手续费

## 核心功能

### Borrow 指令

- 从协议金库转账指定金额到借款人 ATA
- 验证本指令必须是交易中的第一条（index=0）
- 验证交易最后一条指令必须是本程序的 `repay`
- 验证 `repay` 指令的账户参数与本次借款一致

### Repay 指令

- 从交易第一条 `borrow` 指令中读取借款金额
- 计算 5% 手续费
- 从借款人 ATA 转回"本金 + 手续费"到协议金库

## 项目结构

```
anchor-flash-loan/
├── programs/
│   └── anchor-flash-loan/
│       └── src/
│           └── lib.rs          # 主程序实现
├── tests/
│   └── anchor-flash-loan.ts    # 测试套件
├── Anchor.toml                  # Anchor 配置
└── Cargo.toml                   # Rust 依赖配置
```

## 技术栈

- **Rust** - Solana 程序开发语言
- **Anchor** - Solana 开发框架
- **TypeScript** - 测试代码
- **Solana SPL Token** - 代币处理

## 环境要求

- Rust 工具链
- Anchor CLI
- Solana CLI
- Node.js + Yarn

## 安装

```bash
# 安装依赖
yarn install

# 配置 Solana 本地集群
solana-test-validator
```

## 构建

```bash
# 构建程序
anchor build

# 同步程序 ID（如有更改）
anchor keys list
```

## 测试

```bash
# 运行测试套件
anchor test
```

测试覆盖场景：
- ✅ 正常借贷流程（收取 5% 手续费）
- ✅ 拒绝零金额借款
- ✅ 拒绝 borrow 不是第一条指令
- ✅ 拒绝 repay 使用不匹配的 ATA

## 核心机制

### 指令内省

本协议利用 Solana 的 `sysvar::instructions` 来检查同一交易中的其他指令：

```rust
// 获取当前指令索引
let current_index = load_current_index_checked(&ixs)?;

// 获取交易中指令总数
let len = u16::from_le_bytes(instruction_sysvar[0..2]);

// 读取最后一条指令
let repay_ix = load_instruction_at_checked(len as usize - 1, &ixs)?;
```

### 安全验证

1. **指令顺序验证**：`borrow` 必须是 index=0，`repay` 必须是最后一条
2. **程序 ID 验证**：确保 `repay` 指令由本程序发起
3. **指令鉴别符验证**：验证指令数据前 8 字节匹配 `repay` 的 discriminator
4. **账户一致性验证**：确保 `repay` 的 ATA 账户与 `borrow` 一致

## 协议参数

| 参数 | 值 |
|------|-----|
| Program ID | `8B4sLttXDiq1KpGP4sgw2ZvTh81SWQhkzSrtvPYPSXVc` |
| Protocol PDA Seeds | `["protocol"]` |
| Fee Rate | 500 bps (5%) |

## 错误码

| 错误码 | 说明 |
|--------|------|
| `InvalidIx` | 指令顺序或类型不正确 |
| `InvalidInstructionIndex` | 指令索引无效 |
| `InvalidAmount` | 借款金额为 0 |
| `NotEnoughFunds` | 资金不足 |
| `InvalidProgram` | 程序 ID 不匹配 |
| `InvalidBorrowerAta` | 借款人 ATA 不匹配 |
| `InvalidProtocolAta` | 协议 ATA 不匹配 |
| `MissingRepayIx` | 缺少还款指令 |
| `MissingBorrowIx` | 缺少借款指令 |
| `Overflow` | 计算溢出 |

## License

MIT
