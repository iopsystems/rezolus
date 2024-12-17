#ifndef CGROUP_INFO_H
#define CGROUP_INFO_H

#define CGROUP_NAME_LEN 64

struct cgroup_info {
	int id;
	u8 name[CGROUP_NAME_LEN];
	u8 pname[CGROUP_NAME_LEN];
	u8 gpname[CGROUP_NAME_LEN];
};

#endif //CGROUP_INFO_H